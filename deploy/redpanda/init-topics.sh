#!/usr/bin/env bash
set -euo pipefail

BROKER="${REDPANDA_BROKERS:-redpanda:9092}"

echo "[hyperion-redpanda-init] Waiting for Redpanda cluster at ${BROKER}..."
until rpk cluster info --brokers "${BROKER}" >/dev/null 2>&1; do
  sleep 1
done

echo "[hyperion-redpanda-init] Provisioning deterministic HYPERION topics..."

create_topic() {
  local name="$1"
  local partitions="$2"
  local retention_ms="$3"
  local cleanup_policy="${4:-delete}"

  rpk topic create "${name}" \
    --brokers "${BROKER}" \
    --partitions "${partitions}" \
    --replicas 1 \
    --topic-config "retention.ms=${retention_ms}" \
    --topic-config "cleanup.policy=${cleanup_policy}" \
    --topic-config "compression.type=lz4" || true
}

# 24h = 86400000 ms, 48h = 172800000 ms, 72h = 259200000 ms, 7d = 604800000 ms
create_topic "hyperion.orders.commands.v1"          8 86400000  "delete"
create_topic "hyperion.market.l3-deltas.v1"         8 86400000  "delete"
create_topic "hyperion.market.l2-snapshots.v1"      8 86400000  "compact,delete"
create_topic "hyperion.market.executions.v1"        8 259200000 "delete"
create_topic "hyperion.ledger.journals.v1"          8 604800000 "delete"
create_topic "hyperion.analytics.candles.v1"        8 172800000 "delete"
create_topic "hyperion.analytics.microstructure.v1" 8 86400000  "delete"
create_topic "hyperion.fraud.alerts.v1"             4 604800000 "delete"
create_topic "hyperion.risk.mitigations.v1"         4 604800000 "delete"
create_topic "hyperion.sre.incidents.v1"            2 604800000 "delete"

echo "[hyperion-redpanda-init] Topic inventory verified:"
rpk topic list --brokers "${BROKER}"
