#!/usr/bin/env python3
"""Automated TCP & HTTP healthcheck probe for HYPERION local infrastructure and microservices."""

from __future__ import annotations

import argparse
import socket
import sys
import urllib.request

INFRA_TARGETS: list[tuple[str, str, int, str | None]] = [
    ("Redpanda Kafka API", "127.0.0.1", 19092, None),
    ("Redpanda Admin Ready", "127.0.0.1", 19644, "http://127.0.0.1:19644/v1/status/ready"),
    ("TimescaleDB (PG16)", "127.0.0.1", 5432, None),
    ("Redis 7", "127.0.0.1", 6379, None),
    ("OTel Collector Health", "127.0.0.1", 13133, "http://127.0.0.1:13133/"),
    ("Prometheus", "127.0.0.1", 9090, "http://127.0.0.1:9090/-/healthy"),
    ("Jaeger Tracing UI", "127.0.0.1", 16686, "http://127.0.0.1:16686/"),
]

SERVICE_TARGETS: list[tuple[str, str, int, str | None]] = [
    ("gateway-go", "127.0.0.1", 8081, "http://127.0.0.1:8081/healthz"),
    ("ledger-rs (gRPC)", "127.0.0.1", 50151, None),
    ("stream-aggregator", "127.0.0.1", 8083, "http://127.0.0.1:8083/healthz"),
    ("sentinel-ml", "127.0.0.1", 8084, "http://127.0.0.1:8084/healthz"),
    ("aegis-agent", "127.0.0.1", 8085, "http://127.0.0.1:8085/healthz"),
    ("chaos-sim", "127.0.0.1", 8086, "http://127.0.0.1:8086/healthz"),
    ("chronos-replay", "127.0.0.1", 8088, "http://127.0.0.1:8088/healthz"),
]


def check_target(name: str, host: str, port: int, url: str | None) -> tuple[bool, str]:
    try:
        with socket.create_connection((host, port), timeout=2.0):
            pass
    except OSError as exc:
        return False, f"TCP {host}:{port} unreachable ({exc})"

    if url:
        try:
            with urllib.request.urlopen(url, timeout=2.5) as resp:
                if 200 <= resp.status < 300:
                    return True, f"HTTP {resp.status} OK ({url})"
                return False, f"HTTP {resp.status} ({url})"
        except Exception as exc:  # noqa: BLE001
            return False, f"HTTP failed ({exc})"
    return True, f"TCP {host}:{port} OPEN"


def main() -> int:
    parser = argparse.ArgumentParser(description="HYPERION infrastructure & mesh health probe")
    parser.add_argument("--services", action="store_true", help="Also check microservices CH02-CH08")
    args = parser.parse_args()

    targets = list(INFRA_TARGETS)
    if args.services:
        targets.extend(SERVICE_TARGETS)

    all_ok = True
    for name, host, port, url in targets:
        ok, detail = check_target(name, host, port, url)
        badge = "PASS" if ok else "FAIL"
        print(f"[{badge}] {name:<24} -> {detail}")
        if not ok:
            all_ok = False

    return 0 if all_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
