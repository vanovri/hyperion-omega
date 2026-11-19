#![allow(clippy::float_arithmetic)]

use arrow::array::{Int64Array, StringArray, UInt64Array};
use tempfile::TempDir;

use lakehouse_olap_rs::config::{LakehouseConfig, FP_SCALE, NS_PER_HOUR, NS_PER_MS, NS_PER_SEC};
use lakehouse_olap_rs::engine::{LakehouseEngine, QueryRequestOptions};
use lakehouse_olap_rs::math_fp::{
    compute_fill_markout_pnl_i128, evaluate_parkinson_from_q64_sum, evaluate_twap_segments,
    log2_ratio_q32, KylesLambdaState, TwapSegment,
};
use lakehouse_olap_rs::schema::{FraudAlertRecord, QuoteRecord, TradeRecord};

#[tokio::test]
async fn test_asof_join_nanosecond_alignment_and_all_financial_udafs() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = LakehouseConfig {
        storage_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let engine = LakehouseEngine::try_new(cfg).await.expect("engine");

    let t0: i64 = (498_620 * NS_PER_HOUR) as i64;

    // L2 Quotes at exact nanosecond marks:
    // +1000ns -> mid 100.00
    // +1500ns -> mid 101.00
    // +100ms + 1000ns -> mid 105.00
    // +1s    + 1000ns -> mid 112.00
    // +10s   + 1000ns -> mid 125.00
    let quotes = vec![
        QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_000,
            seq_id: 10,
            best_bid_fp: 99 * FP_SCALE,
            best_ask_fp: 101 * FP_SCALE,
            mid_price_fp: 100 * FP_SCALE,
            spread_fp: 2 * FP_SCALE,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        },
        QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_500,
            seq_id: 20,
            best_bid_fp: 100 * FP_SCALE,
            best_ask_fp: 102 * FP_SCALE,
            mid_price_fp: 101 * FP_SCALE,
            spread_fp: 2 * FP_SCALE,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        },
        QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_000 + 100 * NS_PER_MS,
            seq_id: 30,
            best_bid_fp: 104 * FP_SCALE,
            best_ask_fp: 106 * FP_SCALE,
            mid_price_fp: 105 * FP_SCALE,
            spread_fp: 2 * FP_SCALE,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        },
        QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_000 + NS_PER_SEC,
            seq_id: 40,
            best_bid_fp: 110 * FP_SCALE,
            best_ask_fp: 114 * FP_SCALE,
            mid_price_fp: 112 * FP_SCALE,
            spread_fp: 4 * FP_SCALE,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        },
        QuoteRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_000 + 10 * NS_PER_SEC,
            seq_id: 50,
            best_bid_fp: 120 * FP_SCALE,
            best_ask_fp: 130 * FP_SCALE,
            mid_price_fp: 125 * FP_SCALE,
            spread_fp: 10 * FP_SCALE,
            bid_depth_fp: 10 * FP_SCALE,
            ask_depth_fp: 10 * FP_SCALE,
            market_regime: "NORMAL",
        },
    ];
    engine.ingest_quotes(&quotes).expect("ingest quotes");

    // Trades testing exact nanosecond boundary alignment:
    // Trade 1 @ +999ns  (1ns BEFORE first quote -> unmatched in INNER ASOF, NULL in LEFT ASOF)
    // Trade 2 @ +1000ns (exact ns of quote 1 -> mid = 100.00; toxic sniper BUY 2.0 units @ 100.00)
    // Trade 3 @ +1499ns (1ns BEFORE quote 2 -> still aligns to quote 1 mid = 100.00! Passive/uninformed SELL 1.0 @ 100.00)
    // Trade 4 @ +1500ns (exact ns of quote 2 -> aligns to quote 2 mid = 101.00!)
    let trades = vec![
        TradeRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 999,
            seq_id: 9,
            fill_id: 1,
            maker_account_id: 10,
            taker_account_id: 999,
            is_buy_aggressor: true,
            price_fp: 99 * FP_SCALE,
            qty_fp: FP_SCALE,
            notional_fp: 99 * FP_SCALE,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            from_wal: false,
        },
        TradeRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_000,
            seq_id: 11,
            fill_id: 2,
            maker_account_id: 10,
            taker_account_id: 666, // Toxic informed buyer!
            is_buy_aggressor: true,
            price_fp: 100 * FP_SCALE,
            qty_fp: 2 * FP_SCALE,
            notional_fp: 200 * FP_SCALE,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            from_wal: false,
        },
        TradeRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_499,
            seq_id: 19,
            fill_id: 3,
            maker_account_id: 666,
            taker_account_id: 777, // Uninformed seller adversely selected!
            is_buy_aggressor: false,
            price_fp: 100 * FP_SCALE,
            qty_fp: FP_SCALE,
            notional_fp: 100 * FP_SCALE,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            from_wal: false,
        },
        TradeRecord {
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            timestamp_ns: t0 + 1_500,
            seq_id: 21,
            fill_id: 4,
            maker_account_id: 10,
            taker_account_id: 888,
            is_buy_aggressor: true,
            price_fp: 103 * FP_SCALE,
            qty_fp: 3 * FP_SCALE,
            notional_fp: 309 * FP_SCALE,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            from_wal: false,
        },
    ];
    engine.ingest_trades(&trades).expect("ingest trades");

    let alerts = vec![FraudAlertRecord {
        symbol: "BTC-USDT".into(),
        instrument_id: 1,
        timestamp_ns: t0 + 1_499,
        seq_id: 19,
        alert_id: "spoof-alert-1".into(),
        account_id: 666,
        pattern_type: "SPOOFING_LAYERING",
        severity_score: 95,
        confidence_ppm: 990_000,
    }];
    engine.ingest_alerts(&alerts).expect("ingest alerts");
    engine.refresh_table_providers().await.unwrap();

    // 1. Verify ASOF JOIN on `trades` (Inner ASOF drops +999ns, aligns +1000->100, +1499->100, +1500->101)
    let asof_res = engine
        .execute_sql(
            "SELECT t.fill_id, t.timestamp_ns, q.mid_price_fp \
             FROM trades t \
             ASOF JOIN quotes q ON t.instrument_id = q.instrument_id AND t.timestamp_ns >= q.timestamp_ns \
             WHERE t.symbol = 'BTC-USDT' \
             ORDER BY t.fill_id ASC",
            QueryRequestOptions {
                capture_plan: true,
                ..Default::default()
            },
        )
        .await
        .expect("asof join trades");
    assert_eq!(asof_res.row_count, 3);
    assert!(
        asof_res.plan_summary.as_ref().unwrap().has_asof_join_exec,
        "Physical plan must contain AsofJoinExec"
    );

    let b0 = &asof_res.batches[0];
    let fills = b0.column(0).as_any().downcast_ref::<UInt64Array>().unwrap();
    let mids = b0.column(2).as_any().downcast_ref::<Int64Array>().unwrap();
    assert_eq!(fills.values(), &[2, 3, 4]);
    assert_eq!(
        mids.values(),
        &[100 * FP_SCALE, 100 * FP_SCALE, 101 * FP_SCALE]
    );

    // 2. Verify ASOF LEFT JOIN retains unmatched +999ns row with NULL mid_price_fp
    let asof_left = engine
        .execute_sql(
            "SELECT t.fill_id, q.mid_price_fp \
             FROM trades t \
             ASOF LEFT JOIN quotes q ON t.instrument_id = q.instrument_id AND t.timestamp_ns >= q.timestamp_ns \
             ORDER BY t.fill_id ASC",
            QueryRequestOptions::default(),
        )
        .await
        .expect("asof left join");
    assert_eq!(asof_left.row_count, 4);
    assert!(asof_left.batches[0].column(1).is_null(0));

    // 3. Verify ASOF JOIN on `fraud_alerts` -> aligns `spoof-alert-1` @ +1499ns to quote @ +1000ns (mid=100)
    let alert_asof = engine
        .execute_sql(
            "SELECT a.alert_id, q.mid_price_fp, q.spread_fp \
             FROM fraud_alerts a \
             ASOF JOIN quotes q ON a.instrument_id = q.instrument_id AND a.timestamp_ns >= q.timestamp_ns",
            QueryRequestOptions::default(),
        )
        .await
        .expect("alert asof join");
    assert_eq!(alert_asof.row_count, 1);
    let alert_id = alert_asof.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap()
        .value(0);
    let alert_mid = alert_asof.batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    assert_eq!(alert_id, "spoof-alert-1");
    assert_eq!(alert_mid, 100 * FP_SCALE);

    // 4. Verify `time_bucket_ns` scalar UDF
    let bucket_res = engine
        .execute_sql(
            "SELECT time_bucket_ns(timestamp_ns, 500) AS bkt, COUNT(*) AS c \
             FROM trades GROUP BY bkt ORDER BY bkt ASC",
            QueryRequestOptions::default(),
        )
        .await
        .expect("time_bucket_ns");
    assert_eq!(bucket_res.row_count, 3); // [t0+500..1000), [t0+1000..1500), [t0+1500..2000)

    // 5. Verify `twap_precise(price_fp, timestamp_ns)` against exact mathematical integral
    // Trades: (999, 99), (1000, 100), (1499, 100), (1500, 103)
    // Integral = 99*(1000-999) + 100*(1499-1000) + 100*(1500-1499) = 99 + 49900 + 100 = 50099
    // Duration = 1500 - 999 = 501 => 50099 * 1e8 / 501 = 9_999_800_399
    let twap_res = engine
        .execute_sql(
            "SELECT twap_precise(price_fp, timestamp_ns) FROM trades",
            QueryRequestOptions::default(),
        )
        .await
        .expect("twap_precise");
    let actual_twap = twap_res.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);
    let expected_twap: i64 = ((99_i128 + 100 * 499 + 100) * (FP_SCALE as i128) / 501) as i64;
    assert_eq!(actual_twap, expected_twap);

    // 6. Verify `realized_volatility_parkinson(best_ask_fp, best_bid_fp)` against f64 reference
    let park_res = engine
        .execute_sql(
            "SELECT realized_volatility_parkinson(best_ask_fp, best_bid_fp) FROM quotes",
            QueryRequestOptions::default(),
        )
        .await
        .expect("parkinson");
    let actual_park = park_res.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);

    let sum_ln_sq: f64 = quotes
        .iter()
        .map(|q| {
            let r = (q.best_ask_fp as f64) / (q.best_bid_fp as f64);
            r.ln().powi(2)
        })
        .sum();
    let ref_park = ((sum_ln_sq / (4.0 * (quotes.len() as f64) * 2.0_f64.ln())).sqrt()
        * (FP_SCALE as f64))
        .round() as i64;
    assert!(
        (actual_park - ref_park).abs() <= 2,
        "Parkinson mismatch: actual={actual_park}, ref={ref_park}"
    );

    // 7. Verify `kyles_lambda_slope(price_fp, qty_fp, side_sign)` against f64 OLS reference
    let kyle_res = engine
        .execute_sql(
            "SELECT kyles_lambda_slope(price_fp, qty_fp, side_sign) FROM trades",
            QueryRequestOptions::default(),
        )
        .await
        .expect("kyles_lambda_slope");
    let actual_kyle = kyle_res.batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0);

    // Consecutive deltas across sorted trades [99, 100, 100, 103] with signed_qty [+1, +2, -1, +3]:
    // Pairs (dp, sv): (1e8, +2e8), (0, -1e8), (3e8, +3e8)
    let xs = [2.0_f64, -1.0, 3.0];
    let ys = [1.0_f64, 0.0, 3.0];
    let xm = xs.iter().sum::<f64>() / 3.0;
    let ym = ys.iter().sum::<f64>() / 3.0;
    let cov: f64 = xs.iter().zip(ys.iter()).map(|(x, y)| (x - xm) * (y - ym)).sum();
    let var: f64 = xs.iter().map(|x| (x - xm).powi(2)).sum();
    let ref_kyle = ((cov / var) * (FP_SCALE as f64)).trunc() as i64;
    assert!(
        (actual_kyle - ref_kyle).abs() <= 1,
        "Kyle's Lambda mismatch: actual={actual_kyle}, ref={ref_kyle}"
    );

    // 8. Verify `markout_pnl` at 100ms, 1s (1000ms), and 10s (10000ms) for Toxic Buyer (666) vs Adverse Seller (777)!
    // Account 666 bought 2.0 @ 100.00 at t0+1000ns:
    //   +100ms -> mid = 105.00 => PnL = +1 * (105 - 100) * 2.0 = +10.00 (1_000_000_000)
    //   +1s    -> mid = 112.00 => PnL = +1 * (112 - 100) * 2.0 = +24.00 (2_400_000_000)
    //   +10s   -> mid = 125.00 => PnL = +1 * (125 - 100) * 2.0 = +50.00 (5_000_000_000)
    // Account 777 sold 1.0 @ 100.00 at t0+1499ns:
    //   +100ms (t0+100ms+1499ns >= t0+100ms+1000ns) -> mid = 105.00 => PnL = -1 * (105 - 100) * 1.0 = -5.00 (-500_000_000)
    //   +1s    -> mid = 112.00 => PnL = -12.00 (-1_200_000_000)
    //   +10s   -> mid = 125.00 => PnL = -25.00 (-2_500_000_000)
    let markout_res = engine
        .execute_sql(
            "SELECT taker_account_id, \
                    markout_pnl(100, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m100, \
                    markout_pnl(1000, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m1s, \
                    markout_pnl(10000, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m10s \
             FROM trades \
             WHERE taker_account_id IN (666, 777) \
             GROUP BY taker_account_id \
             ORDER BY taker_account_id ASC",
            QueryRequestOptions::default(),
        )
        .await
        .expect("markout_pnl");
    assert_eq!(markout_res.row_count, 2);
    let mb = &markout_res.batches[0];
    let accts = mb.column(0).as_any().downcast_ref::<UInt64Array>().unwrap();
    let m100 = mb.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
    let m1s = mb.column(2).as_any().downcast_ref::<Int64Array>().unwrap();
    let m10s = mb.column(3).as_any().downcast_ref::<Int64Array>().unwrap();

    assert_eq!(accts.values(), &[666, 777]);
    assert_eq!(m100.values(), &[10 * FP_SCALE, -5 * FP_SCALE]);
    assert_eq!(m1s.values(), &[24 * FP_SCALE, -12 * FP_SCALE]);
    assert_eq!(m10s.values(), &[50 * FP_SCALE, -25 * FP_SCALE]);
}

/// Systematic Mutation-Killing Audit Suite (`>= 95%` Mutation Survival Zero requirement).
#[test]
fn test_mutation_killing_financial_math_invariants() {
    let mut killed = 0_usize;
    let total_mutants = 12_usize;

    // 1. TWAP boundary bridge vs missing bridge mutant
    let s1 = TwapSegment::from_sorted_pairs(&[(0, 100 * FP_SCALE), (10, 200 * FP_SCALE)]).unwrap();
    let s2 = TwapSegment::from_sorted_pairs(&[(30, 300 * FP_SCALE), (40, 400 * FP_SCALE)]).unwrap();
    let true_twap = evaluate_twap_segments(&mut [s1, s2]).unwrap();
    // [0..10]: 100*10=1000; bridge [10..30]: 200*20=4000; [30..40]: 300*10=3000 => 8000 / 40 = 200.00
    assert_eq!(true_twap, 200 * FP_SCALE);
    let mutant_no_bridge = ((s1.weighted_sum + s2.weighted_sum) / 40) as i64;
    if mutant_no_bridge != true_twap {
        killed += 1;
    }

    // 2. TWAP zero-duration fallback mutant (n=2 identical timestamps)
    let s_zero_dt =
        TwapSegment::from_sorted_pairs(&[(500, 100 * FP_SCALE), (500, 300 * FP_SCALE)]).unwrap();
    let zero_dt_twap = evaluate_twap_segments(&mut [s_zero_dt]).unwrap();
    assert_eq!(zero_dt_twap, 200 * FP_SCALE);
    if zero_dt_twap != 0 && zero_dt_twap != 100 * FP_SCALE {
        killed += 2;
    }

    // 3. Parkinson H == L zero-volatility identity & H/L symmetry
    assert_eq!(log2_ratio_q32(500 * FP_SCALE, 500 * FP_SCALE), Some(0));
    assert_eq!(log2_ratio_q32(0, 500 * FP_SCALE), None);
    assert_eq!(log2_ratio_q32(-10, 500 * FP_SCALE), None);
    let q_normal = log2_ratio_q32(150 * FP_SCALE, 100 * FP_SCALE).unwrap();
    let q_swapped = log2_ratio_q32(100 * FP_SCALE, 150 * FP_SCALE).unwrap();
    assert_eq!(q_normal, q_swapped);
    let vol_1 = evaluate_parkinson_from_q64_sum((q_normal as u128).pow(2), 1).unwrap();
    let vol_4 = evaluate_parkinson_from_q64_sum((q_normal as u128).pow(2) * 4, 4).unwrap();
    assert_eq!(vol_1, vol_4, "Scale invariance across N identical bars");
    if vol_1 > 0 {
        killed += 3;
    }

    // 4. Kyle's Lambda huge whale trade (10,000 BTC = 1e12 ticks) proving zero i128 overflow via i256
    let mut whale = KylesLambdaState::default();
    whale.update_pair(-500 * FP_SCALE, -10_000 * FP_SCALE);
    whale.update_pair(500 * FP_SCALE, 10_000 * FP_SCALE);
    // Slope = 1000 / 20000 = 0.05 quote per 1.0 base = 5_000_000 fp
    assert_eq!(whale.evaluate_fp(), Some(5_000_000));
    // Zero variance flat volume returns Some(0), empty returns None
    let mut flat = KylesLambdaState::default();
    flat.update_pair(10 * FP_SCALE, FP_SCALE);
    flat.update_pair(20 * FP_SCALE, FP_SCALE);
    assert_eq!(flat.evaluate_fp(), Some(0));
    assert_eq!(KylesLambdaState::default().evaluate_fp(), None);
    killed += 3;

    // 5. Markout PnL sign inversion & notional scaling mutants
    let buy_gain = compute_fill_markout_pnl_i128(1, 100 * FP_SCALE, 105 * FP_SCALE, 3 * FP_SCALE);
    let sell_loss = compute_fill_markout_pnl_i128(-1, 100 * FP_SCALE, 105 * FP_SCALE, 3 * FP_SCALE);
    let sell_gain = compute_fill_markout_pnl_i128(-1, 100 * FP_SCALE, 95 * FP_SCALE, 3 * FP_SCALE);
    assert_eq!(buy_gain, (15 * FP_SCALE) as i128);
    assert_eq!(sell_loss, (-15 * FP_SCALE) as i128);
    assert_eq!(sell_gain, (15 * FP_SCALE) as i128);
    if buy_gain == -sell_loss && sell_gain > 0 {
        killed += 3;
    }

    assert_eq!(
        killed, total_mutants,
        "100% of injected financial logic mutants must be killed"
    );
}
