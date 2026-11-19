#!/usr/bin/env python3
"""HYPERION One-Command Stack Orchestrator (`just up`, `just down`, `make demo`).

Automatically detects Docker Engine availability:
  - Primary mode (`--mode docker` or auto when Docker Engine responds):
      Launches `docker compose -f docker-compose.full.yml up -d --build` and waits
      for every service healthcheck to report healthy.
  - Native compiled-binary mode (`--mode native` or automatic fallback when Docker
      Desktop daemon is stopped):
      Spawns compiled release binaries of all 8 HYPERION services on canonical ports
      (`50151..50154`, `8080..8088`, `3000`) with ordered healthcheck waits and PID
      tracking at `.hyperion-runtime/pids.json` for clean `down`.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[1]
RUNTIME_DIR = REPO_ROOT / ".hyperion-runtime"
PID_FILE = RUNTIME_DIR / "pids.json"
LOG_DIR = RUNTIME_DIR / "logs"
DATA_DIR = RUNTIME_DIR / "ledger-data"


def docker_daemon_ready() -> bool:
    if not shutil.which("docker"):
        return False
    try:
        res = subprocess.run(
            ["docker", "info"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=6,
            check=False,
        )
        return res.returncode == 0
    except Exception:
        return False


def wait_http_ok(name: str, url: str, timeout_s: float = 45.0) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(url, timeout=2.0) as resp:
                if 200 <= resp.status < 400:
                    print(f"[hyperion-up] READY   {name:18s} -> {url} (HTTP {resp.status})")
                    return True
        except Exception:
            pass
        time.sleep(0.5)
    print(f"[hyperion-up] TIMEOUT {name:18s} -> {url}", file=sys.stderr)
    return False


def find_sentinel_python() -> str:
    env_py = os.environ.get("SENTINEL_PYTHON")
    candidates: list[Path] = []
    if env_py:
        candidates.append(Path(env_py))
    candidates.extend([
        REPO_ROOT / "services" / "sentinel-ml" / ".venv" / "Scripts" / "python.exe",
        REPO_ROOT / "services" / "sentinel-ml" / ".venv" / "bin" / "python",
        REPO_ROOT / ".venv" / "Scripts" / "python.exe",
        REPO_ROOT / ".venv" / "bin" / "python",
    ])
    for c in candidates:
        if c.exists():
            return str(c)
    return sys.executable


def find_binary(rel_candidates: list[str]) -> Path | None:
    for rel in rel_candidates:
        p = REPO_ROOT / rel
        if p.exists():
            return p
    return None


def ensure_native_binaries() -> dict[str, Path]:
    bins: dict[str, Path] = {}

    ledger = find_binary([
        "services/ledger-rs/target/release/ledger-rs.exe",
        "services/ledger-rs/target/release/ledger-rs",
    ])
    if ledger is None:
        print("[hyperion-up] Building ledger-rs --release ...")
        subprocess.run(
            ["cargo", "build", "--release", "--bin", "ledger-rs"],
            cwd=REPO_ROOT / "services" / "ledger-rs",
            check=True,
        )
        ledger = find_binary([
            "services/ledger-rs/target/release/ledger-rs.exe",
            "services/ledger-rs/target/release/ledger-rs",
        ])
    assert ledger is not None
    bins["ledger-rs"] = ledger

    agg = find_binary([
        "services/stream-aggregator/target/release/stream-aggregator.exe",
        "services/stream-aggregator/target/release/stream-aggregator",
    ])
    if agg is None:
        print("[hyperion-up] Building stream-aggregator --release ...")
        subprocess.run(
            ["cargo", "build", "--release", "--bin", "stream-aggregator"],
            cwd=REPO_ROOT / "services" / "stream-aggregator",
            check=True,
        )
        agg = find_binary([
            "services/stream-aggregator/target/release/stream-aggregator.exe",
            "services/stream-aggregator/target/release/stream-aggregator",
        ])
    assert agg is not None
    bins["stream-aggregator"] = agg

    chronos = find_binary([
        "services/chronos-replay/target/release/chronos-replay.exe",
        "services/chronos-replay/target/release/chronos-replay",
    ])
    if chronos is None:
        print("[hyperion-up] Building chronos-replay --release ...")
        subprocess.run(
            ["cargo", "build", "--release", "--bin", "chronos-replay"],
            cwd=REPO_ROOT / "services" / "chronos-replay",
            check=True,
        )
        chronos = find_binary([
            "services/chronos-replay/target/release/chronos-replay.exe",
            "services/chronos-replay/target/release/chronos-replay",
        ])
    assert chronos is not None
    bins["chronos-replay"] = chronos

    ext = ".exe" if os.name == "nt" else ""
    gw_out = RUNTIME_DIR / "bin" / f"gateway-go{ext}"
    gw_out.parent.mkdir(parents=True, exist_ok=True)
    if not gw_out.exists():
        print("[hyperion-up] Building gateway-go ...")
        subprocess.run(
            ["go", "build", "-trimpath", "-o", str(gw_out), "./cmd/gateway"],
            cwd=REPO_ROOT / "services" / "gateway-go",
            check=True,
        )
    bins["gateway-go"] = gw_out

    chaos_out = RUNTIME_DIR / "bin" / f"chaos-sim{ext}"
    if not chaos_out.exists():
        print("[hyperion-up] Building chaos-sim ...")
        subprocess.run(
            ["go", "build", "-trimpath", "-o", str(chaos_out), "./cmd/chaos-sim"],
            cwd=REPO_ROOT / "services" / "chaos-sim",
            check=True,
        )
    bins["chaos-sim"] = chaos_out
    return bins


def spawn_service(
    name: str,
    cmd: list[str],
    cwd: Path,
    env_extra: dict[str, str],
) -> subprocess.Popen[bytes]:
    LOG_DIR.mkdir(parents=True, exist_ok=True)
    log_path = LOG_DIR / f"{name}.log"
    log_file = open(log_path, "wb")
    env = os.environ.copy()
    env.update(env_extra)
    creationflags = 0
    if os.name == "nt":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP
    proc = subprocess.Popen(
        cmd,
        cwd=str(cwd),
        env=env,
        stdout=log_file,
        stderr=subprocess.STDOUT,
        creationflags=creationflags,
    )
    print(f"[hyperion-up] STARTED {name:18s} pid={proc.pid} log={log_path.relative_to(REPO_ROOT)}")
    return proc


def up_docker() -> int:
    compose = REPO_ROOT / "docker-compose.full.yml"
    print(f"[hyperion-up] Docker Engine online -> starting {compose.name} ...")
    subprocess.run(
        ["docker", "compose", "-f", str(compose), "up", "-d", "--build"],
        cwd=str(REPO_ROOT),
        check=True,
    )
    endpoints = [
        ("ledger-rs", "http://127.0.0.1:8082/healthz"),
        ("stream-aggregator", "http://127.0.0.1:8083/healthz"),
        ("sentinel-ml", "http://127.0.0.1:8084/healthz"),
        ("chronos-replay", "http://127.0.0.1:8088/healthz"),
        ("gateway-go", "http://127.0.0.1:8081/healthz"),
        ("aegis-agent", "http://127.0.0.1:8085/healthz"),
        ("chaos-sim", "http://127.0.0.1:8086/healthz"),
        ("terminal-web", "http://127.0.0.1:3000/"),
    ]
    all_ok = all(wait_http_ok(name, url, timeout_s=60.0) for name, url in endpoints)
    if all_ok:
        print("\nHYPERION FULL STACK READY:")
        print("  Terminal UI : http://localhost:3000")
        print("  Gateway API : http://localhost:8080  (WS: ws://localhost:8080/ws?format=binary)")
        print("  Grafana     : http://localhost:3002  | Prometheus: http://localhost:9090 | Jaeger: http://localhost:16686")
    return 0 if all_ok else 1


def up_native(skip_ui: bool = False, clean_data: bool = False) -> int:
    if clean_data and DATA_DIR.exists():
        shutil.rmtree(DATA_DIR, ignore_errors=True)
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    bins = ensure_native_binaries()

    sentinel_py = find_sentinel_python()
    sentinel_src = str(REPO_ROOT / "services" / "sentinel-ml" / "src")
    contracts_py = str(REPO_ROOT / "contracts" / "gen" / "python" / "src")
    py_path = f"{sentinel_src}{os.pathsep}{contracts_py}"
    node_exe = shutil.which("node") or "node"
    npm_exe = shutil.which("npm.cmd") or shutil.which("npm") or "npm"

    aegis_dist = REPO_ROOT / "services" / "aegis-agent" / "dist" / "index.js"
    if not aegis_dist.exists():
        print("[hyperion-up] Building aegis-agent ...")
        subprocess.run([npm_exe, "run", "build"], cwd=REPO_ROOT / "services" / "aegis-agent", check=True)

    specs: list[tuple[str, list[str], Path, dict[str, str], str]] = [
        (
            "ledger-rs",
            [str(bins["ledger-rs"])],
            REPO_ROOT / "services" / "ledger-rs",
            {
                "HYPERION_LEDGER_GRPC_ADDR": "127.0.0.1:50151",
                "HYPERION_LEDGER_METRICS_PORT": "8082",
                "HYPERION_DATA_DIR": str(DATA_DIR),
                "RUST_LOG": "info",
            },
            "http://127.0.0.1:8082/healthz",
        ),
        (
            "stream-aggregator",
            [str(bins["stream-aggregator"])],
            REPO_ROOT / "services" / "stream-aggregator",
            {
                "HYPERION_AGGREGATOR_GRPC_ADDR": "127.0.0.1:50152",
                "HYPERION_AGGREGATOR_HTTP_ADDR": "127.0.0.1:8083",
                "ENABLE_EXTERNAL_IO": "false",
                "RUST_LOG": "info",
            },
            "http://127.0.0.1:8083/healthz",
        ),
        (
            "sentinel-ml",
            [sentinel_py, "-m", "sentinel_ml.cli", "serve"],
            REPO_ROOT / "services" / "sentinel-ml",
            {
                "PYTHONPATH": py_path,
                "SENTINEL_MODELS_DIR": str(REPO_ROOT / "services" / "sentinel-ml" / "models"),
                "SENTINEL_GRPC_PORT": "50153",
                "SENTINEL_HTTP_PORT": "8084",
                "HYPERION_REDPANDA_BROKERS": "",
            },
            "http://127.0.0.1:8084/healthz",
        ),
        (
            "chronos-replay",
            [str(bins["chronos-replay"])],
            REPO_ROOT / "services" / "chronos-replay",
            {
                "HYPERION_CHRONOS_REPLAY_GRPC_ADDR": "127.0.0.1:50154",
                "HYPERION_CHRONOS_REPLAY_HTTP_PORT": "8088",
                "HYPERION_DATA_DIR": str(DATA_DIR),
                "RUST_LOG": "info",
            },
            "http://127.0.0.1:8088/healthz",
        ),
        (
            "gateway-go",
            [str(bins["gateway-go"])],
            REPO_ROOT / "services" / "gateway-go",
            {
                "GATEWAY_PUBLIC_ADDR": "127.0.0.1:8080",
                "GATEWAY_ADMIN_ADDR": "127.0.0.1:8081",
                "LEDGER_GRPC_ADDRS": "127.0.0.1:50151",
                "ANALYTICS_GRPC_ADDRS": "127.0.0.1:50152",
                "FRAUD_GRPC_ADDRS": "127.0.0.1:50153",
                "CHRONOS_GRPC_ADDRS": "127.0.0.1:50154",
                "CHAOS_SIM_HTTP_URL": "http://127.0.0.1:8086",
                "REDIS_ADDR": "",
                "ENABLE_REDPANDA": "false",
                "RATELIMIT_DEFAULT_RPS": "5000",
                "RATELIMIT_DEFAULT_BURST": "10000",
            },
            "http://127.0.0.1:8081/healthz",
        ),
        (
            "aegis-agent",
            [node_exe, str(aegis_dist)],
            REPO_ROOT / "services" / "aegis-agent",
            {
                "HYPERION_AEGIS_AGENT_HTTP_PORT": "8085",
                "HYPERION_GATEWAY_URL": "http://127.0.0.1:8080",
                "HYPERION_SENTINEL_ML_URL": "http://127.0.0.1:8084",
                "AEGIS_UPSTREAM_MODE": "hybrid",
                "AEGIS_REASONING_MODE": "deterministic",
            },
            "http://127.0.0.1:8085/healthz",
        ),
        (
            "chaos-sim",
            [str(bins["chaos-sim"]), "--listen", "127.0.0.1:8086", "--backend", "mock", "--bots", "1024"],
            REPO_ROOT / "services" / "chaos-sim",
            {
                "HYPERION_GATEWAY_URL": "http://127.0.0.1:8080",
                "HYPERION_LEDGER_GRPC": "127.0.0.1:50151",
                "CHAOS_ENABLE_PROXIES": "true",
            },
            "http://127.0.0.1:8086/healthz",
        ),
    ]

    if not skip_ui:
        ui_dir = REPO_ROOT / "apps" / "terminal-web"
        next_build = ui_dir / ".next" / "BUILD_ID"
        if not next_build.exists():
            print("[hyperion-up] Building terminal-web (Next.js 15 production bundle) ...")
            subprocess.run([npm_exe, "run", "build"], cwd=ui_dir, check=True)
        next_cli = ui_dir / "node_modules" / "next" / "dist" / "bin" / "next"
        specs.append(
            (
                "terminal-web",
                [node_exe, str(next_cli), "start", "--hostname", "127.0.0.1", "--port", "3000"],
                ui_dir,
                {"NODE_ENV": "production", "PORT": "3000"},
                "http://127.0.0.1:3000/",
            )
        )

    pids: dict[str, Any] = {}
    all_ok = True
    for name, cmd, cwd, env_extra, health_url in specs:
        proc = spawn_service(name, cmd, cwd, env_extra)
        pids[name] = {"pid": proc.pid, "health_url": health_url}
        if not wait_http_ok(name, health_url, timeout_s=35.0):
            all_ok = False
            break

    RUNTIME_DIR.mkdir(parents=True, exist_ok=True)
    PID_FILE.write_text(json.dumps(pids, indent=2), encoding="utf-8")
    if all_ok:
        print("\nHYPERION NATIVE MESH READY (All services healthy on canonical ports):")
        for name, meta in pids.items():
            print(f"  {name:18s} pid={meta['pid']:<7d} {meta['health_url']}")
    return 0 if all_ok else 1


def down_all() -> int:
    if PID_FILE.exists():
        pids = json.loads(PID_FILE.read_text(encoding="utf-8"))
        for name, meta in pids.items():
            pid = int(meta["pid"])
            try:
                if os.name == "nt":
                    subprocess.run(
                        ["taskkill", "/PID", str(pid), "/T", "/F"],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        check=False,
                    )
                else:
                    os.kill(pid, signal.SIGTERM)
                print(f"[hyperion-down] Stopped {name} (pid={pid})")
            except Exception:
                pass
        PID_FILE.unlink(missing_ok=True)

    if docker_daemon_ready():
        compose = REPO_ROOT / "docker-compose.full.yml"
        if compose.exists():
            subprocess.run(
                ["docker", "compose", "-f", str(compose), "down"],
                cwd=str(REPO_ROOT),
                check=False,
            )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="HYPERION one-command stack orchestrator")
    parser.add_argument("action", choices=["up", "down", "status", "demo"])
    parser.add_argument("--mode", choices=["auto", "docker", "native"], default="auto")
    parser.add_argument("--skip-ui", action="store_true", help="Skip Next.js terminal-web in native mode")
    parser.add_argument("--clean-data", action="store_true", help="Reset native ledger WAL directory before boot")
    args = parser.parse_args()

    if args.action == "down":
        return down_all()
    if args.action == "status":
        for name, url in [
            ("ledger-rs", "http://127.0.0.1:8082/healthz"),
            ("gateway-go", "http://127.0.0.1:8081/healthz"),
            ("stream-aggregator", "http://127.0.0.1:8083/healthz"),
            ("sentinel-ml", "http://127.0.0.1:8084/healthz"),
            ("aegis-agent", "http://127.0.0.1:8085/healthz"),
            ("chaos-sim", "http://127.0.0.1:8086/healthz"),
            ("chronos-replay", "http://127.0.0.1:8088/healthz"),
            ("terminal-web", "http://127.0.0.1:3000/"),
        ]:
            wait_http_ok(name, url, timeout_s=1.5)
        return 0

    use_docker = args.mode == "docker" or (args.mode == "auto" and docker_daemon_ready())
    rc = up_docker() if use_docker else up_native(skip_ui=args.skip_ui, clean_data=args.clean_data)
    if rc != 0:
        return rc

    if args.action == "demo":
        print("\n[hyperion-demo] Stack live -> launching 3-Phase End-to-End Chaos Crucible ...")
        crucible = REPO_ROOT / "e2e" / "run_crucible.py"
        res = subprocess.run([sys.executable, str(crucible), "--reuse-running"], cwd=str(REPO_ROOT), check=False)
        print("\n[hyperion-demo] Interactive Terminal UI ready at http://localhost:3000")
        print("[hyperion-demo] Stop stack anytime with: just down (or make down)")
        return res.returncode

    return 0


if __name__ == "__main__":
    sys.exit(main())
