use hyperion_stream_aggregator::candle::ALL_TIMEFRAMES;
use hyperion_stream_aggregator::config::AggregatorConfig;
use hyperion_stream_aggregator::engine::{IngestEvent, IngestOutcome, StreamAggregatorEngine};
use hyperion_stream_aggregator::reference::compute_naive_reference;
use hyperion_stream_aggregator::synthetic::{
    generate_ordered_market_stream, shuffle_within_watermark_bound, SyntheticStreamConfig,
};

#[test]
fn stress_260k_ordered_and_shuffled_matches_naive_oracle_bit_exact() {
    let lateness_ns = 2_000_000_000_u64; // 2.0s deterministic watermark window
    let stream_cfg = SyntheticStreamConfig {
        symbol: "BTC-USDT".to_string(),
        seed: 0x2026_1118_0004_CAFE,
        event_count: 260_000,
        start_ts_ns: 1_795_020_000_000_000_000,
        step_ts_ns: 200_000, // 260k * ~0.2ms = ~52 seconds span
        anchor_price_fp: 6_850_000_000_000,
    };

    let ordered_events = generate_ordered_market_stream(&stream_cfg);
    assert_eq!(ordered_events.len(), 260_000);

    // 1. Compute Naive O(N log N) Reference Oracle
    let expected = compute_naive_reference(&ordered_events, "BTC-USDT", lateness_ns);
    assert!(expected.total_trades > 130_000);

    // 2. Run StreamAggregatorEngine on Ordered 260,000-event stream
    let cfg = AggregatorConfig {
        watermark_lateness_ns: lateness_ns,
        hot_candle_ring_capacity: 2048,
        ..Default::default()
    };

    let mut eng_ordered = StreamAggregatorEngine::with_raw_collection(cfg.clone(), false);
    for ev in &ordered_events {
        let out = eng_ordered.ingest_event(ev);
        assert_eq!(out, IngestOutcome::Accepted);
    }
    eng_ordered.flush_all_watermarks();

    // 3. Build Partially Shuffled 260,000-event stream (out-of-order up to 400ms < 2s watermark)
    //    PLUS injected duplicate packets and late-arriving out-of-watermark events!
    let mut adversarial_stream =
        shuffle_within_watermark_bound(&ordered_events, 400_000_000, 0xDEAD_BEEF_1234_5678);

    // Inject 500 exact duplicate packets mid-stream
    for i in (1000..60_000).step_by(118) {
        let dup = adversarial_stream[i].clone();
        adversarial_stream.insert(i + 5, dup);
    }

    // Append 25 late-arriving events with timestamps older than final watermark
    let stale_ts = stream_cfg.start_ts_ns + 1_000_000; // ~51 seconds behind stream tail!
    for k in 0..25_u64 {
        if let IngestEvent::Execution(mut f) = ordered_events[2].clone() {
            f.fill_id = 99_000_000 + k;
            f.engine_seq = 99_000_000 + k;
            f.exec_ts_ns = stale_ts + k * 1000;
            adversarial_stream.push(IngestEvent::Execution(f));
        }
    }

    let mut eng_shuffled = StreamAggregatorEngine::with_raw_collection(cfg, false);
    for ev in &adversarial_stream {
        let _ = eng_shuffled.ingest_event(ev);
    }
    eng_shuffled.flush_all_watermarks();

    assert_eq!(eng_shuffled.total_accepted_events(), 260_000);
    assert_eq!(eng_shuffled.total_duplicate_events(), 500);
    assert_eq!(eng_shuffled.total_late_events(), 25);

    // 4. Verify 100% bit-exact OHLCV, VWAP, TWAP, Trade Count, and Taker Buy/Sell across ALL 6 timeframes
    for tf in ALL_TIMEFRAMES {
        let got_ord = eng_ordered.query_candles("BTC-USDT", tf, 0, u64::MAX, 5000);
        let got_shuf = eng_shuffled.query_candles("BTC-USDT", tf, 0, u64::MAX, 5000);
        let exp_tf: Vec<_> = expected
            .candles
            .iter()
            .filter(|((t, _), _)| *t == tf)
            .map(|(_, c)| c.clone())
            .collect();

        assert_eq!(
            got_ord.len(),
            exp_tf.len(),
            "candle count mismatch for timeframe {:?}",
            tf
        );
        assert_eq!(
            got_ord, exp_tf,
            "ordered OHLCV/VWAP/TWAP mismatch vs naive oracle for {:?}",
            tf
        );
        assert_eq!(
            got_shuf, exp_tf,
            "shuffled out-of-order OHLCV/VWAP/TWAP mismatch vs naive oracle for {:?}",
            tf
        );
    }

    // 5. Verify Microstructure VWAP, Cont-Kukanov-Stoikov OFI, and Book Imbalance 5/10/20
    let pipe_ord = eng_ordered.pipeline("BTC-USDT").expect("BTC-USDT pipeline");
    let pipe_shuf = eng_shuffled.pipeline("BTC-USDT").expect("BTC-USDT pipeline");

    assert_eq!(pipe_ord.micro().session_vwap_fp(), expected.session_vwap_fp);
    assert_eq!(pipe_shuf.micro().session_vwap_fp(), expected.session_vwap_fp);
    assert_eq!(pipe_ord.micro().rolling_vwap_fp(), expected.rolling_vwap_fp);
    assert_eq!(pipe_shuf.micro().rolling_vwap_fp(), expected.rolling_vwap_fp);

    assert_eq!(
        pipe_ord.micro().cumulative_ofi_fp(),
        expected.cumulative_ofi_fp
    );
    assert_eq!(
        pipe_shuf.micro().cumulative_ofi_fp(),
        expected.cumulative_ofi_fp
    );
    assert_eq!(pipe_ord.micro().rolling_ofi_fp(), expected.rolling_ofi_fp);
    assert_eq!(pipe_shuf.micro().rolling_ofi_fp(), expected.rolling_ofi_fp);

    let m_ord = eng_ordered.query_latest_microstructure("BTC-USDT").unwrap();
    let m_shuf = eng_shuffled
        .query_latest_microstructure("BTC-USDT")
        .unwrap();
    assert_eq!(
        m_ord.book_imbalance_top5_ppm,
        expected.book_imbalance_top5_ppm
    );
    assert_eq!(
        m_ord.book_imbalance_top10_ppm,
        expected.book_imbalance_top10_ppm
    );
    assert_eq!(
        m_ord.book_imbalance_top20_ppm,
        expected.book_imbalance_top20_ppm
    );
    assert_eq!(m_shuf.book_imbalance_top5_ppm, m_ord.book_imbalance_top5_ppm);
    assert_eq!(
        m_shuf.book_imbalance_top10_ppm,
        m_ord.book_imbalance_top10_ppm
    );
    assert_eq!(
        m_shuf.book_imbalance_top20_ppm,
        m_ord.book_imbalance_top20_ppm
    );
}
