use std::time::Instant;

use arrow::array::Int64Array;
use tempfile::TempDir;

use lakehouse_olap_rs::config::LakehouseConfig;
use lakehouse_olap_rs::engine::{LakehouseEngine, QueryRequestOptions};
use lakehouse_olap_rs::synth_bench::generate_million_row_parquet_dataset;

#[tokio::test(flavor = "multi_thread")]
async fn test_two_million_row_parquet_pruning_pushdown_and_sub_150ms_benchmark() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = LakehouseConfig {
        storage_root: tmp.path().to_path_buf(),
        parquet_row_group_size: 131_072,
        execution_batch_size: 65_536,
        ..Default::default()
    };

    let engine = LakehouseEngine::try_new(cfg).await.expect("engine");

    // 1. Generate 2,000,000 synthetic trades + 400,000 L2 quotes = 2,400,000 Parquet rows across 8 partitions!
    let stats = generate_million_row_parquet_dataset(&engine, 2_000_000, 400_000)
        .await
        .expect("generate 2.4M synthetic rows");

    println!(
        "[CH17 SYNTH DATASET] Generated {} trades + {} quotes = {} rows across {} partitions ({:.2} MiB Zstd Parquet) in {} ms",
        stats.total_trade_rows,
        stats.total_quote_rows,
        stats.total_rows,
        stats.partitions_written,
        (stats.total_parquet_bytes as u64) / (1024 * 1024),
        stats.generation_elapsed_ms
    );
    assert!(stats.total_trade_rows >= 2_000_000);
    assert_eq!(stats.partitions_written, 8);

    // 2. Verify Partition Pruning + Predicate Pushdown via DataFusion PhysicalExecutionPlan inspection!
    // Note: `SELECT COUNT(*) FROM trades` is answered purely from Parquet footer statistics (`0` files scanned),
    // whereas `SUM(notional_fp)` forces column reads across all 8 partition files before pruning!
    let full_plan = engine
        .explain_physical_plan("SELECT COUNT(*), SUM(notional_fp) FROM trades")
        .await
        .expect("explain full");
    assert_eq!(
        full_plan.total_parquet_files_scheduled, 8,
        "Unfiltered column scan should schedule all 8 partition files"
    );

    let pruned_sql = format!(
        "SELECT COUNT(*), SUM(notional_fp) FROM trades \
         WHERE symbol = 'BTC-USDT' AND hour_bucket = 498620 AND taker_account_id = {}",
        stats.toxic_sniper_account_id
    );
    let pruned_plan = engine
        .explain_physical_plan(&pruned_sql)
        .await
        .expect("explain pruned");

    println!(
        "[CH17 PHYSICAL PLAN - PRUNED]\n{}",
        pruned_plan.physical_plan_tree
    );
    assert!(pruned_plan.has_parquet_scan);
    assert!(
        pruned_plan.has_pushed_predicate,
        "Expected Parquet predicate pushdown (`predicate=...`) in PhysicalExecutionPlan"
    );
    assert_eq!(
        pruned_plan.total_parquet_files_scheduled, 1,
        "Partition pruning MUST prune 7 of 8 partitions, scheduling exactly 1 Parquet file!"
    );

    // 3. Verify Toxic Sniper detection via `markout_pnl` at 100ms, 1s, and 10s on the 2.2M dataset!
    let sniper_sql = format!(
        "SELECT COUNT(*) AS fills, \
                markout_pnl(100, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m100_fp, \
                markout_pnl(1000, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m1s_fp, \
                markout_pnl(10000, instrument_id, timestamp_ns, price_fp, qty_fp, side_sign) AS m10s_fp \
         FROM trades \
         WHERE symbol = 'BTC-USDT' AND taker_account_id = {}",
        stats.toxic_sniper_account_id
    );
    let sniper_res = engine
        .execute_sql(&sniper_sql, QueryRequestOptions::default())
        .await
        .expect("sniper markout query");
    let sb = &sniper_res.batches[0];
    let sniper_fills = sb.column(0).as_any().downcast_ref::<Int64Array>().unwrap().value(0);
    let sniper_m100 = sb.column(1).as_any().downcast_ref::<Int64Array>().unwrap().value(0);
    let sniper_m1s = sb.column(2).as_any().downcast_ref::<Int64Array>().unwrap().value(0);
    let sniper_m10s = sb.column(3).as_any().downcast_ref::<Int64Array>().unwrap().value(0);
    println!(
        "[CH17 TOXIC FLOW SNIPER] fills={sniper_fills}, m100_fp={sniper_m100}, m1s_fp={sniper_m1s}, m10s_fp={sniper_m10s}, latency={} us",
        sniper_res.elapsed_us
    );
    assert!(sniper_fills > 0);
    assert!(
        sniper_m100 > 0 && sniper_m1s > 0 && sniper_m10s > 0,
        "Informed sniper preceding jump ticks must exhibit strictly positive markout PnL"
    );

    // 4. Full 2,000,000-Row Aggregation Latency Loop (< 150 ms hard target!)
    let full_agg_sql = "SELECT symbol, \
                               COUNT(*) AS trade_cnt, \
                               SUM(qty_fp) AS total_qty_fp, \
                               SUM(notional_fp) AS total_notional_fp, \
                               twap_precise(price_fp, timestamp_ns) AS twap_fp \
                        FROM trades \
                        GROUP BY symbol \
                        ORDER BY symbol ASC";

    // Warmup pass (populates OS page cache & thread pool, captures execution metrics)
    let warmup = engine
        .execute_sql(
            full_agg_sql,
            QueryRequestOptions {
                capture_plan: true,
                ..Default::default()
            },
        )
        .await
        .expect("warmup 2M agg");
    if let Some(ref ps) = warmup.plan_summary {
        println!("[CH17 2M AGG EXECUTION METRICS]\n{}", ps.execution_metrics_text);
    }
    assert_eq!(warmup.row_count, 4);
    let total_counted: i64 = warmup
        .batches
        .iter()
        .flat_map(|b| {
            b.column(1)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap()
                .values()
                .iter()
                .copied()
        })
        .sum();
    assert_eq!(total_counted, 2_000_000);

    // Measure 5 timed iterations and take min & median (p50)
    let mut samples_us = Vec::with_capacity(5);
    for _ in 0..5 {
        let t_iter = Instant::now();
        let out = engine
            .execute_sql(full_agg_sql, QueryRequestOptions::default())
            .await
            .expect("2M full agg");
        assert_eq!(out.row_count, 4);
        samples_us.push(t_iter.elapsed().as_micros() as u64);
    }
    samples_us.sort_unstable();
    let min_ms = samples_us[0] / 1_000;
    let p50_ms = samples_us[2] / 1_000;
    println!(
        "[CH17 2M-ROW AGGREGATION BENCHMARK] samples_us={samples_us:?} | min={min_ms} ms ({} us) | p50={p50_ms} ms ({} us)",
        samples_us[0], samples_us[2]
    );

    assert!(
        p50_ms < 150,
        "2,000,000-row aggregation latency p50 ({p50_ms} ms) MUST be strictly < 150 ms!"
    );
}
