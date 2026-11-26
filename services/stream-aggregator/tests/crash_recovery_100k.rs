use hyperion_stream_aggregator::broker::{ExactlyOnceCoordinator, InMemoryBrokerCluster};
use hyperion_stream_aggregator::candle::ALL_TIMEFRAMES;
use hyperion_stream_aggregator::config::{topics, AggregatorConfig};
use hyperion_stream_aggregator::engine::{EngineError, StreamAggregatorEngine};
use hyperion_stream_aggregator::reference::compute_naive_reference;
use hyperion_stream_aggregator::storage::{AnalyticsStorage, InMemoryTransactionalStore};
use hyperion_stream_aggregator::synthetic::{
    generate_ordered_market_stream, SyntheticStreamConfig,
};
use std::sync::Arc;

#[tokio::test]
async fn crash_recovery_mid_100k_events_zero_volume_duplication() {
    let stream_cfg = SyntheticStreamConfig {
        symbol: "ETH-USDT".to_string(),
        seed: 0xC0FF_EE42_1000_0001,
        event_count: 100_000,
        start_ts_ns: 1_795_030_000_000_000_000,
        step_ts_ns: 500_000, // 100k * 0.5ms = 50 seconds across multiple 1s/5s/1m candles
        anchor_price_fp: 345_000_000_000, // 3,450.00 USDT
    };
    let events = generate_ordered_market_stream(&stream_cfg);
    assert_eq!(events.len(), 100_000);

    let lateness_ns = 1_500_000_000_u64;
    let oracle = compute_naive_reference(&events, "ETH-USDT", lateness_ns);

    // Populate partitioned Redpanda simulator (8 partitions, murmur2 keyed)
    let broker = Arc::new(InMemoryBrokerCluster::new());
    for ev in &events {
        broker.produce_ingest_event(8, ev);
    }

    let store = Arc::new(InMemoryTransactionalStore::new());
    let cfg = AggregatorConfig {
        consumer_group: "hyperion-crash-test-group".to_string(),
        watermark_lateness_ns: lateness_ns,
        batch_flush_max_events: 2_500,
        hot_candle_ring_capacity: 2048,
        ..Default::default()
    };

    let source_topics = [
        topics::MARKET_L2_SNAPSHOTS,
        topics::MARKET_EXECUTIONS,
        topics::MARKET_L3_DELTAS,
    ];

    // --- PHASE 1: Consumer #1 processes 53,719 events (21 committed batches = 52,500 events,
    //              plus 1,219 uncommitted dirty in-memory events mid-candle!) then CRASHES! ---
    {
        let mut consumer_1 =
            ExactlyOnceCoordinator::new(cfg.clone(), Arc::clone(&store), Arc::clone(&broker), None);
        let read_count = consumer_1
            .drain_in_memory_broker(&broker, &source_topics, 53_719, false)
            .await
            .expect("phase 1 drain");
        assert_eq!(read_count, 53_719);

        // Simulate catastrophic DB crash right as Consumer #1 tries an emergency flush, then kill Consumer #1!
        store.arm_fail_next_commit();
        let failed_flush = consumer_1.flush_now(false).await;
        assert!(
            failed_flush.is_err(),
            "armed DB fault must abort transaction cleanly"
        );
        drop(consumer_1);
    }

    // --- PHASE 2: Spawn brand-new Consumer #2, recover from last ACID checkpoint,
    //              inject duplicate replay of 1,500 already-committed broker records,
    //              and finish consuming up to 100,000 events! ---
    let mut consumer_2 =
        ExactlyOnceCoordinator::new(cfg.clone(), Arc::clone(&store), Arc::clone(&broker), None);
    let recovered = consumer_2
        .recover_from_storage(&cfg.consumer_group)
        .await
        .expect("recover checkpoint");
    assert!(recovered, "checkpoint snapshot must exist in store");

    // Deliberately replay older broker records from offset 0 (simulating broker offset reset / network replay)
    for topic in source_topics {
        for part in broker.partitions_for(topic) {
            let replay_slice = broker.fetch_from(topic, part, 0, 400);
            if !replay_slice.is_empty() {
                consumer_2
                    .process_records(&replay_slice)
                    .await
                    .expect("idempotent replay processing");
            }
        }
    }

    // Now drain remaining uncommitted events to completion and seal watermarks
    let resumed_count = consumer_2
        .drain_in_memory_broker(&broker, &source_topics, 100_000, true)
        .await
        .expect("phase 2 drain to completion");
    assert!(
        resumed_count > 46_000,
        "consumer #2 must re-process uncommitted tail from 52,500..100,000"
    );

    // --- PHASE 3: Mathematical Proof of ZERO Volume Duplication & Exact Candle Equivalence ---
    for tf in ALL_TIMEFRAMES {
        let committed_vol = store.total_committed_candle_volume_fp("ETH-USDT", tf);
        assert_eq!(
            committed_vol, oracle.total_base_volume_fp,
            "ZERO DUPLICATION VIOLATION on timeframe {:?}: committed={} != oracle={}",
            tf, committed_vol, oracle.total_base_volume_fp
        );

        let db_candles = store
            .query_candles("ETH-USDT", tf, 0, u64::MAX, 5_000)
            .await
            .expect("query committed candles");
        let exp_candles: Vec<_> = oracle
            .candles
            .iter()
            .filter(|((t, _), _)| *t == tf)
            .map(|(_, c)| c.clone())
            .collect();

        assert_eq!(
            db_candles, exp_candles,
            "post-recovery candle mismatch on {:?}",
            tf
        );
    }

    assert_eq!(
        store.committed_execution_count("ETH-USDT") as u64,
        oracle.total_trades
    );
    assert!(
        !broker.published_candles().is_empty(),
        "real-time candle updates must be published to hyperion.analytics.candles.v1"
    );
    assert!(
        !broker.published_microstructure().is_empty(),
        "real-time microstructure updates must be published to hyperion.analytics.microstructure.v1"
    );
}

#[test]
fn checkpoint_corruption_rejected_by_crc32c_and_blake3() {
    let cfg = AggregatorConfig::default();
    let engine = StreamAggregatorEngine::new(cfg.clone());
    let mut blob = engine.serialize_checkpoint().expect("serialize");
    let last = blob.len() - 1;
    blob[last] ^= 0xFF;

    let mut target = StreamAggregatorEngine::new(cfg);
    let err = target.restore_from_checkpoint(&blob).unwrap_err();
    assert!(matches!(err, EngineError::CorruptedCheckpoint));
}
