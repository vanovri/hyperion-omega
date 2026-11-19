use prost::Message;
use tempfile::TempDir;

use lakehouse_olap_rs::compactor::{inspect_parquet_segment, BackgroundCompactorHandle};
use lakehouse_olap_rs::config::{LakehouseConfig, FP_SCALE, NS_PER_HOUR};
use lakehouse_olap_rs::engine::{LakehouseEngine, QueryRequestOptions};
use lakehouse_olap_rs::error::LakehouseError;
use lakehouse_olap_rs::wal_ingest::{
    decode_redpanda_execution_batch, decode_redpanda_fraud_alert_batch,
    decode_redpanda_l2_snapshot_batch, encode_wal_segment, parse_cold_wal_segment, wire,
};

#[tokio::test]
async fn test_wal_and_redpanda_zero_copy_ingestion_and_compaction() {
    let tmp = TempDir::new().expect("tempdir");
    let cfg = LakehouseConfig {
        storage_root: tmp.path().to_path_buf(),
        compaction_min_files: 3,
        compaction_interval: std::time::Duration::from_millis(80),
        ..Default::default()
    };

    let engine = LakehouseEngine::try_new(cfg.clone()).await.expect("engine");
    let base_ts: u64 = 498_620 * NS_PER_HOUR;

    // 1. Build Cold WAL segment (`HYPWAL01`) with out-of-order timestamps inside the same hour
    let wal_envelopes = vec![
        wire::MarketEventEnvelopeWire {
            engine_seq: 1,
            symbol: "BTC-USDT".into(),
            ts_ns: base_ts + 5_000_000,
            payload: Some(wire::EnvelopePayloadWire::L2Snapshot(
                wire::L2BookSnapshotWire {
                    engine_seq: 1,
                    symbol: "BTC-USDT".into(),
                    instrument_id: 1,
                    bids: vec![wire::PriceLevelWire {
                        price_fp: 6_800_000_000_000,
                        total_qty_fp: 200_000_000,
                        order_count: 3,
                    }],
                    asks: vec![wire::PriceLevelWire {
                        price_fp: 6_800_200_000_000,
                        total_qty_fp: 150_000_000,
                        order_count: 2,
                    }],
                    best_bid_fp: 6_800_000_000_000,
                    best_ask_fp: 6_800_200_000_000,
                    mid_price_fp: 6_800_100_000_000,
                    spread_fp: 200_000_000,
                    snapshot_ts_ns: base_ts + 5_000_000,
                    state_root_blake3: vec![0x11; 32],
                },
            )),
        },
        wire::MarketEventEnvelopeWire {
            engine_seq: 2,
            symbol: "BTC-USDT".into(),
            ts_ns: base_ts + 9_000_000,
            payload: Some(wire::EnvelopePayloadWire::Fill(wire::ExecutionFillWire {
                fill_id: 5002,
                engine_seq: 2,
                symbol: "BTC-USDT".into(),
                instrument_id: 1,
                maker_order_id: 10,
                taker_order_id: 11,
                maker_account_id: 1001,
                taker_account_id: 2002,
                aggressor_side: 1, // BUY
                price_fp: 6_800_200_000_000,
                qty_fp: 50_000_000,
                notional_fp: None,
                maker_fee_fp: -68_002,
                taker_fee_fp: 340_010,
                exec_ts_ns: base_ts + 9_000_000,
                wal_blake3_hash: vec![0x22; 32],
            })),
        },
        wire::MarketEventEnvelopeWire {
            engine_seq: 3,
            symbol: "BTC-USDT".into(),
            ts_ns: base_ts + 7_000_000, // Deliberately out-of-order timestamp to verify composite sort!
            payload: Some(wire::EnvelopePayloadWire::Fill(wire::ExecutionFillWire {
                fill_id: 5003,
                engine_seq: 3,
                symbol: "BTC-USDT".into(),
                instrument_id: 1,
                maker_order_id: 12,
                taker_order_id: 13,
                maker_account_id: 1003,
                taker_account_id: 2004,
                aggressor_side: 2, // SELL
                price_fp: 6_800_000_000_000,
                qty_fp: 25_000_000,
                notional_fp: None,
                maker_fee_fp: -34_000,
                taker_fee_fp: 170_000,
                exec_ts_ns: base_ts + 7_000_000,
                wal_blake3_hash: vec![0x33; 32],
            })),
        },
    ];

    let wal_bytes = encode_wal_segment(&wal_envelopes, [0_u8; 32]);

    // 2. Verify WAL CRC32C & Blake3 tamper detection
    let mut corrupted_crc = wal_bytes.clone();
    let last_idx = corrupted_crc.len() - 1;
    corrupted_crc[last_idx] ^= 0xFF;
    assert!(matches!(
        parse_cold_wal_segment(&corrupted_crc, Some([0_u8; 32])),
        Err(LakehouseError::WalIntegrity(_))
    ));

    let mut bad_magic = wal_bytes.clone();
    bad_magic[0] = b'X';
    assert!(matches!(
        parse_cold_wal_segment(&bad_magic, None),
        Err(LakehouseError::WalIntegrity(_))
    ));

    // 3. Ingest valid Cold WAL segment (creates L0 file #1 in BTC-USDT/hour_bucket=498620)
    let (frames, segs1) = engine
        .ingest_cold_wal_segment(&wal_bytes, Some([0_u8; 32]))
        .expect("ingest cold wal");
    assert_eq!(frames, 3);
    assert_eq!(segs1.len(), 2); // 1 trade segment + 1 quote segment

    // Inspect physical Parquet properties on the trade segment
    let trade_seg = segs1
        .iter()
        .find(|s| s.table_name == "trades")
        .expect("trade seg");
    let inspection = inspect_parquet_segment(&trade_seg.file_path).expect("inspect parquet");
    assert!(inspection.all_zstd_compressed, "Expected ZSTD on all columns");
    assert_eq!(
        inspection.sorting_column_indices,
        vec![0, 1, 2],
        "Expected composite SortingColumns (instrument_id, timestamp_ns, seq_id)"
    );
    assert!(
        inspection
            .bloom_filter_columns
            .contains(&"maker_account_id".to_string())
            && inspection
                .bloom_filter_columns
                .contains(&"taker_account_id".to_string())
            && inspection
                .bloom_filter_columns
                .contains(&"account_id".to_string()),
        "Expected SBBF Bloom filters on account columns, got {:?}",
        inspection.bloom_filter_columns
    );
    assert!(
        inspection
            .dictionary_encoded_columns
            .contains(&"aggressor_side".to_string()),
        "Expected Dictionary encoding on aggressor_side"
    );
    assert_eq!(
        inspection.row_group_ts_bounds,
        vec![((base_ts + 7_000_000) as i64, (base_ts + 9_000_000) as i64)],
        "Expected exact min/max zone-map bounds on timestamp_ns"
    );

    // 4. Ingest two Redpanda batches into the SAME partition (including a duplicate replay of seq=2!)
    // so total L0 files in BTC-USDT/hour_bucket=498620 reaches 3, triggering Background Compactor!
    let rp_batch_1 = vec![
        wire::ExecutionFillWire {
            fill_id: 5002, // Duplicate replay of WAL seq=2!
            engine_seq: 2,
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            maker_order_id: 10,
            taker_order_id: 11,
            maker_account_id: 1001,
            taker_account_id: 2002,
            aggressor_side: 1,
            price_fp: 6_800_200_000_000,
            qty_fp: 50_000_000,
            notional_fp: None,
            maker_fee_fp: -68_002,
            taker_fee_fp: 340_010,
            exec_ts_ns: base_ts + 9_000_000,
            wal_blake3_hash: vec![],
        }
        .encode_to_vec(),
        wire::ExecutionFillWire {
            fill_id: 5004,
            engine_seq: 4,
            symbol: "BTC-USDT".into(),
            instrument_id: 1,
            maker_order_id: 14,
            taker_order_id: 15,
            maker_account_id: 1005,
            taker_account_id: 2006,
            aggressor_side: 1,
            price_fp: 6_800_500_000_000,
            qty_fp: FP_SCALE as u64,
            notional_fp: None,
            maker_fee_fp: 0,
            taker_fee_fp: 0,
            exec_ts_ns: base_ts + 12_000_000,
            wal_blake3_hash: vec![],
        }
        .encode_to_vec(),
    ];
    let rp_trades_1 = decode_redpanda_execution_batch(&rp_batch_1).expect("decode rp1");
    engine.ingest_trades(&rp_trades_1).expect("ingest rp1");

    let rp_batch_2 = vec![wire::ExecutionFillWire {
        fill_id: 5005,
        engine_seq: 5,
        symbol: "BTC-USDT".into(),
        instrument_id: 1,
        maker_order_id: 16,
        taker_order_id: 17,
        maker_account_id: 1007,
        taker_account_id: 2008,
        aggressor_side: 2,
        price_fp: 6_800_300_000_000,
        qty_fp: 2 * FP_SCALE as u64,
        notional_fp: None,
        maker_fee_fp: 0,
        taker_fee_fp: 0,
        exec_ts_ns: base_ts + 15_000_000,
        wal_blake3_hash: vec![],
    }
    .encode_to_vec()];
    let rp_trades_2 = decode_redpanda_execution_batch(&rp_batch_2).expect("decode rp2");
    engine.ingest_trades(&rp_trades_2).expect("ingest rp2");

    // Also decode Redpanda L2 snapshots and Fraud Alerts
    let rp_snaps = decode_redpanda_l2_snapshot_batch(&[wire::L2BookSnapshotWire {
        engine_seq: 6,
        symbol: "ETH-USDT".into(),
        instrument_id: 2,
        bids: vec![],
        asks: vec![],
        best_bid_fp: 345_000_000_000,
        best_ask_fp: 345_100_000_000,
        mid_price_fp: 345_050_000_000,
        spread_fp: 100_000_000,
        snapshot_ts_ns: base_ts + 20_000_000,
        state_root_blake3: vec![],
    }
    .encode_to_vec()])
    .expect("decode snap");
    engine.ingest_quotes(&rp_snaps).expect("ingest snap");

    let rp_alerts = decode_redpanda_fraud_alert_batch(&[wire::AnomalyAlertWire {
        alert_id: "alrt-001".into(),
        engine_seq: 7,
        symbol: "BTC-USDT".into(),
        pattern_type: 1, // WASH_TRADING_CYCLE
        severity_score: 92,
        confidence_ppm: 985_000,
        offender_account_ids: vec![2002, 1001],
        detected_ts_ns: base_ts + 16_000_000,
    }
    .encode_to_vec()])
    .expect("decode alert");
    engine.ingest_alerts(&rp_alerts).expect("ingest alert");

    // 5. Spawn automatic Background Compactor and wait for 1 sweep
    let compactor = BackgroundCompactorHandle::spawn(cfg.clone(), None);
    tokio::time::sleep(std::time::Duration::from_millis(220)).await;
    compactor.shutdown().await;

    let btc_part_dir = cfg
        .storage_root
        .join("trades")
        .join("symbol=BTC-USDT")
        .join("hour_bucket=498620");
    let remaining_files: Vec<_> = std::fs::read_dir(&btc_part_dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".parquet"))
        .collect();
    assert_eq!(
        remaining_files.len(),
        1,
        "Expected 3 L0 fragments compacted into 1 single segment"
    );
    let compacted_fname = remaining_files[0].file_name().to_string_lossy().to_string();
    assert!(compacted_fname.starts_with("compacted_"));

    // 6. Refresh providers & query compacted segment: verify 4 unique deduplicated rows (seq 2, 3, 4, 5)
    // ordered strictly ascending by timestamp_ns (seq 3 @ +7ms comes BEFORE seq 2 @ +9ms!)
    engine.refresh_table_providers().await.unwrap();
    let res = engine
        .execute_sql(
            "SELECT seq_id, timestamp_ns, price_fp FROM trades WHERE symbol = 'BTC-USDT' ORDER BY timestamp_ns ASC",
            QueryRequestOptions::default(),
        )
        .await
        .expect("query compacted trades");
    assert_eq!(res.row_count, 4, "Duplicate seq=2 replay must be deduplicated during compaction");
}
