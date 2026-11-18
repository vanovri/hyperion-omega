#!/usr/bin/env python3
"""HYPERION Channel 01 zero-defect contract, invariant, SQL, and Docker validator."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path
from typing import Any

import sqlparse
import yaml

ROOT = Path(__file__).resolve().parent.parent


def fail(msg: str) -> None:
    print(f"[VALIDATE FAIL] {msg}", file=sys.stderr)
    raise SystemExit(1)


def validate_protos() -> None:
    proto_dir = ROOT / "proto"
    res = subprocess.run(["buf", "lint", str(proto_dir)], capture_output=True, text=True)
    if res.returncode != 0:
        fail(f"buf lint failed:\n{res.stdout}\n{res.stderr}")

    expected_messages = {
        "orderbook.proto": [
            "SubmitOrder",
            "CancelOrder",
            "ModifyOrder",
            "OrderAck",
            "ExecutionFill",
            "L2BookSnapshot",
            "L3BookDelta",
            "MerkleLedgerProof",
        ],
        "telemetry_fraud.proto": [
            "TransactionGraphEdge",
            "AnomalyAlert",
            "WashTradingCycle",
            "SpoofingLayering",
            "VelocitySpike",
            "SHAPFeatureAttribution",
            "RiskMitigationCommand",
        ],
        "chronos.proto": [
            "TimeTravelSeekRequest",
            "TimelineForkRequest",
            "StateTreeDiffResponse",
            "ReplayFrameStream",
        ],
    }

    float_token_re = re.compile(r"^\s*(?:optional\s+|repeated\s+)?(float|double)\s+\w+\s*=", re.MULTILINE)
    for proto_path in sorted(proto_dir.glob("*.proto")):
        raw = proto_path.read_text(encoding="utf-8")
        # Strip line comments before scanning for forbidden scalar types.
        uncommented = "\n".join(line.split("//", 1)[0] for line in raw.splitlines())
        bad = float_token_re.search(uncommented)
        if bad:
            fail(f"Forbidden floating-point field in {proto_path.name}: {bad.group(0).strip()}")

        for req_msg in expected_messages.get(proto_path.name, []):
            if not re.search(rf"\bmessage\s+{req_msg}\b", uncommented):
                fail(f"Missing required message `{req_msg}` in {proto_path.name}")

    print("[validate] Protobuf v3 lint, message inventory, and zero-float invariant: OK")


def resolve_json_pointer(doc: dict[str, Any], ref: str) -> Any:
    if not ref.startswith("#/"):
        fail(f"External $ref unsupported in standalone contract check: {ref}")
    cur: Any = doc
    for part in ref[2:].split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        if not isinstance(cur, dict) or part not in cur:
            fail(f"Broken OpenAPI $ref pointer: {ref}")
        cur = cur[part]
    return cur


def walk_refs(node: Any, root_doc: dict[str, Any]) -> int:
    count = 0
    if isinstance(node, dict):
        if "$ref" in node and isinstance(node["$ref"], str):
            resolve_json_pointer(root_doc, node["$ref"])
            count += 1
        # Enforce zero float/number on any *_fp monetary/quantity field
        props = node.get("properties")
        if isinstance(props, dict):
            for prop_name, prop_schema in props.items():
                if prop_name.endswith("_fp") and isinstance(prop_schema, dict):
                    if prop_schema.get("type") == "number":
                        fail(f"Floating-point OpenAPI property forbidden on fixed-point field `{prop_name}`")
        for v in node.values():
            count += walk_refs(v, root_doc)
    elif isinstance(node, list):
        for item in node:
            count += walk_refs(item, root_doc)
    return count


def validate_openapi_and_ws() -> None:
    oas_path = ROOT / "contracts" / "gateway-openapi.yaml"
    doc = yaml.safe_load(oas_path.read_text(encoding="utf-8"))
    if doc.get("openapi") != "3.1.0":
        fail(f"Expected openapi: 3.1.0, got {doc.get('openapi')}")

    paths = doc.get("paths", {})
    if len(paths) < 8:
        fail(f"Expected comprehensive REST paths in gateway-openapi.yaml, found {len(paths)}")

    op_ids: set[str] = set()
    for path_key, path_item in paths.items():
        for method in ("get", "post", "put", "patch", "delete"):
            if method in path_item:
                op = path_item[method]
                op_id = op.get("operationId")
                if not op_id:
                    fail(f"Missing operationId on {method.upper()} {path_key}")
                if op_id in op_ids:
                    fail(f"Duplicate operationId `{op_id}`")
                op_ids.add(op_id)

    ref_count = walk_refs(doc, doc)

    ws_spec = (ROOT / "contracts" / "ws-binary-protocol.md").read_text(encoding="utf-8")
    for token in ("0x4859", "Little-Endian", "MSG_L2_SNAPSHOT", "MSG_L3_DELTA_BATCH", "MSG_EXECUTION_FILL"):
        if token not in ws_spec:
            fail(f"ws-binary-protocol.md missing required wire token `{token}`")

    print(f"[validate] OpenAPI 3.1.0 ({len(op_ids)} operations, {ref_count} $refs resolved) + WS Binary Spec: OK")


def validate_sql_schema() -> None:
    sql_path = ROOT / "deploy" / "sql" / "001_init_timescaledb.sql"
    raw_sql = sql_path.read_text(encoding="utf-8")
    uncommented = "\n".join(line.split("--", 1)[0] for line in raw_sql.splitlines())

    if re.search(r"\b(FLOAT|FLOAT4|FLOAT8|REAL|DOUBLE\s+PRECISION)\b", uncommented, re.IGNORECASE):
        fail("Forbidden floating-point SQL column type detected in 001_init_timescaledb.sql")

    statements = [s for s in sqlparse.split(uncommented) if s.strip()]
    if len(statements) < 20:
        fail(f"Unexpectedly few SQL statements parsed ({len(statements)})")

    for required_obj in (
        "create_hypertable",
        "add_compression_policy",
        "add_retention_policy",
        "timescaledb.continuous",
        "add_continuous_aggregate_policy",
        "market_executions",
        "l3_book_events",
        "ohlcv_candles",
        "microstructure_metrics",
        "liquidity_heatmap_buckets",
        "stream_consumer_checkpoints",
    ):
        if required_obj not in uncommented:
            fail(f"SQL migration missing required TimescaleDB feature/table `{required_obj}`")

    print(f"[validate] TimescaleDB SQL migration ({len(statements)} statements, hypertables + caggs + 0 floats): OK")


def validate_docker_and_observability() -> None:
    compose_path = ROOT / "docker-compose.infra.yml"
    res = subprocess.run(
        ["docker", "compose", "-f", str(compose_path), "config", "--quiet"],
        capture_output=True,
        text=True,
    )
    if res.returncode != 0:
        fail(f"docker compose config validation failed:\n{res.stdout}\n{res.stderr}")

    compose_doc = yaml.safe_load(compose_path.read_text(encoding="utf-8"))
    services = compose_doc.get("services", {})
    for req_svc in ("redpanda", "redpanda-init", "timescaledb", "redis", "otel-collector", "prometheus", "jaeger"):
        if req_svc not in services:
            fail(f"docker-compose.infra.yml missing required service `{req_svc}`")

    # Verify no host port collides with Windows reserved exclusion range 50000-50059
    for svc_name, svc_cfg in services.items():
        for mapping in svc_cfg.get("ports", []):
            host_port = int(str(mapping).split(":")[0])
            if 50000 <= host_port <= 50059:
                fail(f"Service {svc_name} binds Windows-excluded port {host_port}")

    otel_doc = yaml.safe_load((ROOT / "deploy" / "otel" / "otel-collector-config.yaml").read_text(encoding="utf-8"))
    assert "traces" in otel_doc["service"]["pipelines"], "OTel missing traces pipeline"
    assert "metrics" in otel_doc["service"]["pipelines"], "OTel missing metrics pipeline"

    prom_doc = yaml.safe_load((ROOT / "deploy" / "prometheus" / "prometheus.yml").read_text(encoding="utf-8"))
    assert len(prom_doc.get("scrape_configs", [])) >= 4, "Prometheus missing scrape configs"

    print("[validate] docker-compose.infra.yml, OpenTelemetry Collector, and Prometheus configs: OK")


def main() -> int:
    validate_protos()
    validate_openapi_and_ws()
    validate_sql_schema()
    validate_docker_and_observability()
    print("[validate] ALL HYPERION CHANNEL 01 CONTRACT & INFRA CHECKS PASSED 100%.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
