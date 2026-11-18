-- =============================================================================
-- HYPERION — TIMESCALEDB (POSTGRESQL 16+) INITIALIZATION & HYPERTABLE SCHEMA
-- =============================================================================
-- INVARIANT: ZERO FLOAT / REAL / DOUBLE PRECISION FOR MONETARY OR QUANTITY DATA.
-- All prices, quantities, fees, and balances use BIGINT (64-bit, scale 1e8) or
-- NUMERIC(38, 0) (exact 128-bit+ integer atomic units, scale 1e8).
-- =============================================================================

CREATE EXTENSION IF NOT EXISTS timescaledb;

-- -----------------------------------------------------------------------------
-- 1. EXACTLY-ONCE CONSUMER OFFSET CHECKPOINTS (CH04 Transactional Offset Store)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS stream_consumer_checkpoints (
    consumer_group       TEXT        NOT NULL,
    topic                TEXT        NOT NULL,
    partition_id         INTEGER     NOT NULL CHECK (partition_id >= 0),
    last_kafka_offset    BIGINT      NOT NULL CHECK (last_kafka_offset >= -1),
    last_engine_seq      BIGINT      NOT NULL CHECK (last_engine_seq >= 0),
    watermark_ts_ns      BIGINT      NOT NULL CHECK (watermark_ts_ns >= 0),
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (consumer_group, topic, partition_id)
);

-- -----------------------------------------------------------------------------
-- 2. MARKET EXECUTIONS HYPERTABLE (Idempotent Fill Ledger)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS market_executions (
    exec_time            TIMESTAMPTZ    NOT NULL,
    exec_ts_ns           BIGINT         NOT NULL CHECK (exec_ts_ns > 0),
    symbol               TEXT           NOT NULL,
    instrument_id        INTEGER        NOT NULL,
    engine_seq           BIGINT         NOT NULL CHECK (engine_seq > 0),
    fill_id              BIGINT         NOT NULL CHECK (fill_id > 0),
    maker_order_id       BIGINT         NOT NULL,
    taker_order_id       BIGINT         NOT NULL,
    maker_account_id     BIGINT         NOT NULL,
    taker_account_id     BIGINT         NOT NULL,
    aggressor_side       SMALLINT       NOT NULL CHECK (aggressor_side IN (1, 2)),
    price_fp             BIGINT         NOT NULL CHECK (price_fp > 0),
    qty_fp               BIGINT         NOT NULL CHECK (qty_fp > 0),
    notional_fp          NUMERIC(38, 0) NOT NULL CHECK (notional_fp >= 0),
    maker_fee_fp         BIGINT         NOT NULL,
    taker_fee_fp         BIGINT         NOT NULL,
    wal_blake3_hash      BYTEA          NOT NULL,
    PRIMARY KEY (symbol, exec_time, engine_seq, fill_id)
);

SELECT create_hypertable(
    'market_executions',
    by_range('exec_time', INTERVAL '1 hour'),
    if_not_exists => TRUE
);

CREATE INDEX IF NOT EXISTS idx_market_executions_maker_time
    ON market_executions (maker_account_id, exec_time DESC);
CREATE INDEX IF NOT EXISTS idx_market_executions_taker_time
    ON market_executions (taker_account_id, exec_time DESC);
CREATE INDEX IF NOT EXISTS idx_market_executions_seq
    ON market_executions (symbol, engine_seq DESC);

ALTER TABLE market_executions SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol',
    timescaledb.compress_orderby = 'exec_time DESC, engine_seq DESC'
);
SELECT add_compression_policy('market_executions', INTERVAL '6 hours', if_not_exists => TRUE);
SELECT add_retention_policy('market_executions', INTERVAL '30 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 3. LEVEL-3 ORDER BOOK DELTAS HYPERTABLE
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS l3_book_events (
    event_time           TIMESTAMPTZ NOT NULL,
    event_ts_ns          BIGINT      NOT NULL CHECK (event_ts_ns > 0),
    symbol               TEXT        NOT NULL,
    engine_seq           BIGINT      NOT NULL CHECK (engine_seq > 0),
    prev_engine_seq      BIGINT      NOT NULL CHECK (prev_engine_seq >= 0),
    delta_op             SMALLINT    NOT NULL CHECK (delta_op BETWEEN 1 AND 5),
    side                 SMALLINT    NOT NULL CHECK (side IN (1, 2)),
    order_id             BIGINT      NOT NULL,
    account_id           BIGINT      NOT NULL,
    price_fp             BIGINT      NOT NULL CHECK (price_fp >= 0),
    remaining_qty_fp     BIGINT      NOT NULL CHECK (remaining_qty_fp >= 0),
    delta_qty_fp         BIGINT      NOT NULL,
    PRIMARY KEY (symbol, event_time, engine_seq)
);

SELECT create_hypertable(
    'l3_book_events',
    by_range('event_time', INTERVAL '1 hour'),
    if_not_exists => TRUE
);

ALTER TABLE l3_book_events SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol',
    timescaledb.compress_orderby = 'event_time DESC, engine_seq DESC'
);
SELECT add_compression_policy('l3_book_events', INTERVAL '2 hours', if_not_exists => TRUE);
SELECT add_retention_policy('l3_book_events', INTERVAL '7 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 4. MULTI-TIMEFRAME OHLCV CANDLESTICKS HYPERTABLE (1s, 5s, 1m, 5m, 15m, 1h)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS ohlcv_candles (
    bucket_time              TIMESTAMPTZ    NOT NULL,
    bucket_open_ts_ns        BIGINT         NOT NULL,
    bucket_close_ts_ns       BIGINT         NOT NULL,
    symbol                   TEXT           NOT NULL,
    interval_code            TEXT           NOT NULL CHECK (interval_code IN ('1s', '5s', '1m', '5m', '15m', '1h')),
    open_fp                  BIGINT         NOT NULL CHECK (open_fp > 0),
    high_fp                  BIGINT         NOT NULL CHECK (high_fp >= open_fp AND high_fp >= low_fp),
    low_fp                   BIGINT         NOT NULL CHECK (low_fp > 0 AND low_fp <= open_fp),
    close_fp                 BIGINT         NOT NULL CHECK (close_fp >= low_fp AND close_fp <= high_fp),
    base_volume_fp           NUMERIC(38, 0) NOT NULL CHECK (base_volume_fp >= 0),
    quote_volume_fp          NUMERIC(38, 0) NOT NULL CHECK (quote_volume_fp >= 0),
    taker_buy_base_vol_fp    NUMERIC(38, 0) NOT NULL CHECK (taker_buy_base_vol_fp >= 0),
    taker_sell_base_vol_fp   NUMERIC(38, 0) NOT NULL CHECK (taker_sell_base_vol_fp >= 0),
    vwap_fp                  BIGINT         NOT NULL CHECK (vwap_fp > 0),
    twap_fp                  BIGINT         NOT NULL CHECK (twap_fp > 0),
    trade_count              BIGINT         NOT NULL CHECK (trade_count >= 0),
    last_engine_seq          BIGINT         NOT NULL CHECK (last_engine_seq >= 0),
    is_closed                BOOLEAN        NOT NULL DEFAULT FALSE,
    PRIMARY KEY (symbol, interval_code, bucket_time)
);

SELECT create_hypertable(
    'ohlcv_candles',
    by_range('bucket_time', INTERVAL '6 hours'),
    if_not_exists => TRUE
);

ALTER TABLE ohlcv_candles SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol, interval_code',
    timescaledb.compress_orderby = 'bucket_time DESC'
);
SELECT add_compression_policy('ohlcv_candles', INTERVAL '24 hours', if_not_exists => TRUE);
SELECT add_retention_policy('ohlcv_candles', INTERVAL '90 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 5. QUANTITATIVE MICROSTRUCTURE METRICS HYPERTABLE (OFI, Imbalance, Kyle's Lambda)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS microstructure_metrics (
    metric_time              TIMESTAMPTZ    NOT NULL,
    ts_ns                    BIGINT         NOT NULL,
    symbol                   TEXT           NOT NULL,
    engine_seq               BIGINT         NOT NULL,
    mid_price_fp             BIGINT         NOT NULL CHECK (mid_price_fp >= 0),
    spread_fp                BIGINT         NOT NULL CHECK (spread_fp >= 0),
    spread_bps               INTEGER        NOT NULL CHECK (spread_bps >= 0),
    rolling_vwap_fp          BIGINT         NOT NULL CHECK (rolling_vwap_fp >= 0),
    rolling_twap_fp          BIGINT         NOT NULL CHECK (rolling_twap_fp >= 0),
    ofi_fp                   NUMERIC(38, 0) NOT NULL,
    book_imbalance_top5_ppm  BIGINT         NOT NULL CHECK (book_imbalance_top5_ppm BETWEEN -1000000 AND 1000000),
    book_imbalance_top10_ppm BIGINT         NOT NULL CHECK (book_imbalance_top10_ppm BETWEEN -1000000 AND 1000000),
    book_imbalance_top20_ppm BIGINT         NOT NULL CHECK (book_imbalance_top20_ppm BETWEEN -1000000 AND 1000000),
    kyles_lambda_fp          BIGINT         NOT NULL,
    PRIMARY KEY (symbol, metric_time, engine_seq)
);

SELECT create_hypertable(
    'microstructure_metrics',
    by_range('metric_time', INTERVAL '2 hours'),
    if_not_exists => TRUE
);

ALTER TABLE microstructure_metrics SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol',
    timescaledb.compress_orderby = 'metric_time DESC, engine_seq DESC'
);
SELECT add_compression_policy('microstructure_metrics', INTERVAL '6 hours', if_not_exists => TRUE);
SELECT add_retention_policy('microstructure_metrics', INTERVAL '14 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 6. LIQUIDITY HEATMAP MATRIX HYPERTABLE (Time x Price Bucket Depth)
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS liquidity_heatmap_buckets (
    bucket_time              TIMESTAMPTZ NOT NULL,
    bucket_ts_ns             BIGINT      NOT NULL,
    symbol                   TEXT        NOT NULL,
    price_bucket_fp          BIGINT      NOT NULL CHECK (price_bucket_fp > 0),
    resting_bid_qty_fp       BIGINT      NOT NULL CHECK (resting_bid_qty_fp >= 0),
    resting_ask_qty_fp       BIGINT      NOT NULL CHECK (resting_ask_qty_fp >= 0),
    PRIMARY KEY (symbol, bucket_time, price_bucket_fp)
);

SELECT create_hypertable(
    'liquidity_heatmap_buckets',
    by_range('bucket_time', INTERVAL '2 hours'),
    if_not_exists => TRUE
);

ALTER TABLE liquidity_heatmap_buckets SET (
    timescaledb.compress,
    timescaledb.compress_segmentby = 'symbol',
    timescaledb.compress_orderby = 'bucket_time DESC, price_bucket_fp ASC'
);
SELECT add_compression_policy('liquidity_heatmap_buckets', INTERVAL '4 hours', if_not_exists => TRUE);
SELECT add_retention_policy('liquidity_heatmap_buckets', INTERVAL '7 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 7. FRAUD ANOMALY ALERTS & AUDIT JOURNAL HYPERTABLES
-- -----------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS fraud_anomaly_alerts (
    detected_time            TIMESTAMPTZ NOT NULL,
    detected_ts_ns           BIGINT      NOT NULL,
    alert_id                 TEXT        NOT NULL,
    engine_seq               BIGINT      NOT NULL,
    symbol                   TEXT        NOT NULL,
    pattern_type             TEXT        NOT NULL,
    severity_score           INTEGER     NOT NULL CHECK (severity_score BETWEEN 0 AND 100),
    confidence_ppm           INTEGER     NOT NULL CHECK (confidence_ppm BETWEEN 0 AND 1000000),
    offender_account_ids     BIGINT[]    NOT NULL,
    inference_latency_us     BIGINT      NOT NULL CHECK (inference_latency_us >= 0),
    model_version            TEXT        NOT NULL,
    shap_attributions_json   JSONB       NOT NULL DEFAULT '[]'::jsonb,
    evidence_subgraph_json   JSONB       NOT NULL DEFAULT '[]'::jsonb,
    PRIMARY KEY (symbol, detected_time, alert_id)
);

SELECT create_hypertable(
    'fraud_anomaly_alerts',
    by_range('detected_time', INTERVAL '1 day'),
    if_not_exists => TRUE
);
SELECT add_retention_policy('fraud_anomaly_alerts', INTERVAL '365 days', if_not_exists => TRUE);

CREATE TABLE IF NOT EXISTS ledger_journal_audit (
    journal_time             TIMESTAMPTZ    NOT NULL,
    ts_ns                    BIGINT         NOT NULL,
    journal_seq              BIGINT         NOT NULL,
    engine_seq               BIGINT         NOT NULL,
    reference_type           TEXT           NOT NULL,
    reference_id             BIGINT         NOT NULL,
    total_debit_fp           NUMERIC(38, 0) NOT NULL CHECK (total_debit_fp >= 0),
    total_credit_fp          NUMERIC(38, 0) NOT NULL CHECK (total_credit_fp = total_debit_fp),
    prev_journal_blake3      BYTEA          NOT NULL,
    journal_blake3           BYTEA          NOT NULL,
    lines_json               JSONB          NOT NULL,
    PRIMARY KEY (journal_time, journal_seq)
);

SELECT create_hypertable(
    'ledger_journal_audit',
    by_range('journal_time', INTERVAL '1 day'),
    if_not_exists => TRUE
);
SELECT add_retention_policy('ledger_journal_audit', INTERVAL '365 days', if_not_exists => TRUE);

-- -----------------------------------------------------------------------------
-- 8. CONTINUOUS AGGREGATES (1m, 5m, 1h Real-Time Materialized Views)
-- -----------------------------------------------------------------------------
CREATE MATERIALIZED VIEW IF NOT EXISTS cagg_executions_1m
WITH (timescaledb.continuous) AS
SELECT
    time_bucket(INTERVAL '1 minute', exec_time) AS bucket_1m,
    symbol,
    first(price_fp, exec_time)                  AS open_fp,
    max(price_fp)                               AS high_fp,
    min(price_fp)                               AS low_fp,
    last(price_fp, exec_time)                   AS close_fp,
    sum(qty_fp)::NUMERIC(38, 0)                 AS base_volume_fp,
    sum(notional_fp)::NUMERIC(38, 0)            AS quote_volume_fp,
    sum(CASE WHEN aggressor_side = 1 THEN qty_fp ELSE 0 END)::NUMERIC(38, 0) AS taker_buy_vol_fp,
    sum(CASE WHEN aggressor_side = 2 THEN qty_fp ELSE 0 END)::NUMERIC(38, 0) AS taker_sell_vol_fp,
    count(*)                                    AS trade_count
FROM market_executions
GROUP BY bucket_1m, symbol
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'cagg_executions_1m',
    start_offset      => INTERVAL '2 hours',
    end_offset        => INTERVAL '5 seconds',
    schedule_interval => INTERVAL '5 seconds',
    if_not_exists     => TRUE
);

CREATE MATERIALIZED VIEW IF NOT EXISTS cagg_executions_5m
WITH (timescaledb.continuous) AS
SELECT
    time_bucket(INTERVAL '5 minutes', exec_time) AS bucket_5m,
    symbol,
    first(price_fp, exec_time)                   AS open_fp,
    max(price_fp)                                AS high_fp,
    min(price_fp)                                AS low_fp,
    last(price_fp, exec_time)                    AS close_fp,
    sum(qty_fp)::NUMERIC(38, 0)                  AS base_volume_fp,
    sum(notional_fp)::NUMERIC(38, 0)             AS quote_volume_fp,
    count(*)                                     AS trade_count
FROM market_executions
GROUP BY bucket_5m, symbol
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'cagg_executions_5m',
    start_offset      => INTERVAL '6 hours',
    end_offset        => INTERVAL '10 seconds',
    schedule_interval => INTERVAL '15 seconds',
    if_not_exists     => TRUE
);

CREATE MATERIALIZED VIEW IF NOT EXISTS cagg_executions_1h
WITH (timescaledb.continuous) AS
SELECT
    time_bucket(INTERVAL '1 hour', exec_time) AS bucket_1h,
    symbol,
    first(price_fp, exec_time)                AS open_fp,
    max(price_fp)                             AS high_fp,
    min(price_fp)                             AS low_fp,
    last(price_fp, exec_time)                 AS close_fp,
    sum(qty_fp)::NUMERIC(38, 0)               AS base_volume_fp,
    sum(notional_fp)::NUMERIC(38, 0)          AS quote_volume_fp,
    count(*)                                  AS trade_count
FROM market_executions
GROUP BY bucket_1h, symbol
WITH NO DATA;

SELECT add_continuous_aggregate_policy(
    'cagg_executions_1h',
    start_offset      => INTERVAL '24 hours',
    end_offset        => INTERVAL '1 minute',
    schedule_interval => INTERVAL '1 minute',
    if_not_exists     => TRUE
);
