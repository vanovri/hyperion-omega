#!/usr/bin/env python3
"""
HYPERION OMEGA — Channel 11 Multi-Language Protobuf & gRPC Code Generator
Generates & verifies Rust, Go, Python, and TypeScript bindings for `/proto/omega/*.proto`
while proving ZERO regression on Channel 01 schemas (`/proto/*.proto`).
"""

from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

OMEGA_DIR = Path(__file__).resolve().parent
PROTO_DIR = OMEGA_DIR.parent
ROOT = PROTO_DIR.parent

CH01_PROTO_FILES = [
    "common.proto",
    "orderbook.proto",
    "telemetry_fraud.proto",
    "chronos.proto",
    "stream_analytics.proto",
]

OMEGA_PROTO_FILES = [
    "omega/consensus.proto",
    "omega/zk_solvency.proto",
    "omega/margin_risk.proto",
    "omega/wasm_quant.proto",
    "omega/settlement_l2.proto",
]


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def snapshot_ch01_hashes() -> dict[str, str]:
    hashes: dict[str, str] = {}
    for rel in CH01_PROTO_FILES:
        p = PROTO_DIR / rel
        if p.exists():
            hashes[f"proto/{rel}"] = sha256_file(p)
    ch01_gen = ROOT / "contracts" / "gen"
    if ch01_gen.exists():
        for f in sorted(ch01_gen.rglob("*")):
            if f.is_file() and "node_modules" not in f.parts and "target" not in f.parts and "__pycache__" not in f.parts:
                hashes[str(f.relative_to(ROOT)).replace("\\", "/")] = sha256_file(f)
    return hashes


def run(cmd: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None) -> None:
    rel = cwd.relative_to(ROOT) if cwd != ROOT and ROOT in cwd.parents else cwd
    print(f"[omega-codegen] ({rel}) $ {' '.join(cmd)}", flush=True)
    merged_env = os.environ.copy()
    cargo_bin = str(Path.home() / ".cargo" / "bin")
    merged_env["PATH"] = cargo_bin + os.pathsep + merged_env.get("PATH", "")
    if env:
        merged_env.update(env)
    subprocess.run(cmd, cwd=str(cwd), env=merged_env, check=True)


def lint_and_build_descriptor() -> None:
    run(["buf", "lint", str(PROTO_DIR)], cwd=PROTO_DIR)
    out_desc = OMEGA_DIR / "omega_descriptor.binpb"
    run(["buf", "build", str(PROTO_DIR), "-o", str(out_desc)], cwd=PROTO_DIR)


def codegen_go() -> None:
    go_root = OMEGA_DIR / "gen" / "go"
    go_out = go_root / "omegav1"
    if go_out.exists():
        shutil.rmtree(go_out)
    go_out.mkdir(parents=True, exist_ok=True)

    # Generate into temp staging to flatten `omega/*.pb.go` into `gen/go/omegav1/*.pb.go`
    tmp_out = go_root / "_tmp_pb"
    if tmp_out.exists():
        shutil.rmtree(tmp_out)
    tmp_out.mkdir(parents=True, exist_ok=True)

    run(
        [
            "protoc",
            f"-I={PROTO_DIR}",
            f"--go_out={tmp_out}",
            "--go_opt=paths=source_relative",
            "--go_opt=Mcommon.proto=github.com/vanovri/hyperion/contracts/gen/go/hyperionv1",
            "--go_opt=Morderbook.proto=github.com/vanovri/hyperion/contracts/gen/go/hyperionv1",
            f"--go-grpc_out={tmp_out}",
            "--go-grpc_opt=paths=source_relative",
            "--go-grpc_opt=Mcommon.proto=github.com/vanovri/hyperion/contracts/gen/go/hyperionv1",
            "--go-grpc_opt=Morderbook.proto=github.com/vanovri/hyperion/contracts/gen/go/hyperionv1",
            *OMEGA_PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )

    for generated in (tmp_out / "omega").glob("*.go"):
        shutil.move(str(generated), str(go_out / generated.name))
    shutil.rmtree(tmp_out)

    run(["go", "mod", "tidy"], cwd=go_root)
    run(["go", "vet", "./..."], cwd=go_root)
    go_test_cmd = ["go", "test", "-race", "-v", "./..."] if os.environ.get("HYPERION_GO_RACE") == "1" else ["go", "test", "-v", "./..."]
    run(go_test_cmd, cwd=go_root)


def codegen_python() -> None:
    py_pkg = OMEGA_DIR / "gen" / "python" / "src" / "hyperion_omega_proto"
    py_pkg.mkdir(parents=True, exist_ok=True)
    for stale in py_pkg.glob("*_pb2*"):
        stale.unlink()

    tmp_py = OMEGA_DIR / "gen" / "python" / "_tmp_py"
    if tmp_py.exists():
        shutil.rmtree(tmp_py)
    tmp_py.mkdir(parents=True, exist_ok=True)

    run(
        [
            sys.executable,
            "-m",
            "grpc_tools.protoc",
            f"-I={PROTO_DIR}",
            f"--python_out={tmp_py}",
            f"--pyi_out={tmp_py}",
            f"--grpc_python_out={tmp_py}",
            *OMEGA_PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )

    for f in (tmp_py / "omega").glob("*_pb2*"):
        text = f.read_text(encoding="utf-8")
        # Rewrite CH01 imports (`import common_pb2 as ...`) -> `from hyperion_proto import common_pb2 as ...`
        text = re.sub(
            r"^import (common_pb2|orderbook_pb2|chronos_pb2|stream_analytics_pb2|telemetry_fraud_pb2) as ([A-Za-z0-9_]+)$",
            r"from hyperion_proto import \1 as \2",
            text,
            flags=re.MULTILINE,
        )
        # Rewrite intra-omega imports (`from omega import zk_solvency_pb2 as ...`) -> `from . import zk_solvency_pb2 as ...`
        text = re.sub(
            r"^from omega import ([A-Za-z0-9_]+_pb2) as ([A-Za-z0-9_]+)$",
            r"from . import \1 as \2",
            text,
            flags=re.MULTILINE,
        )
        (py_pkg / f.name).write_text(text, encoding="utf-8")

    shutil.rmtree(tmp_py)
    run([sys.executable, "-m", "pytest", "-v", str(OMEGA_DIR / "gen" / "python" / "tests")], cwd=OMEGA_DIR)


def codegen_typescript() -> None:
    ts_root = OMEGA_DIR / "gen" / "ts"
    npm_cmd = "npm.cmd" if os.name == "nt" else "npm"
    ch01_nm = ROOT / "contracts" / "gen" / "ts" / "node_modules"
    local_nm = ts_root / "node_modules"

    if not local_nm.exists():
        if ch01_nm.exists():
            if os.name == "nt":
                subprocess.run(["cmd", "/c", "mklink", "/J", str(local_nm), str(ch01_nm)], check=True)
            else:
                local_nm.symlink_to(ch01_nm, target_is_directory=True)
        else:
            run([npm_cmd, "install", "--no-audit", "--no-fund"], cwd=ts_root)

    ts_gen = ts_root / "src" / "generated"
    if ts_gen.exists():
        shutil.rmtree(ts_gen)
    ts_gen.mkdir(parents=True, exist_ok=True)

    plugin_name = "protoc-gen-ts_proto.cmd" if os.name == "nt" else "protoc-gen-ts_proto"
    plugin_path = local_nm / ".bin" / plugin_name

    run(
        [
            "protoc",
            f"--plugin=protoc-gen-ts_proto={plugin_path}",
            f"-I={PROTO_DIR}",
            f"--ts_proto_out={ts_gen}",
            "--ts_proto_opt=forceLong=bigint,esModuleInterop=true,outputServices=generic-definitions,exportCommonSymbols=false,env=both",
            "common.proto",
            "orderbook.proto",
            *OMEGA_PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )
    run([npm_cmd, "run", "build"], cwd=ts_root)
    run([npm_cmd, "run", "typecheck"], cwd=ts_root)
    run([npm_cmd, "test"], cwd=ts_root)


def codegen_rust() -> None:
    rs_root = OMEGA_DIR / "gen" / "rust"
    try:
        run(["cargo", "test", "--offline"], cwd=rs_root, env={"HYPERION_OMEGA_REGEN_PROTO": "1"})
    except subprocess.CalledProcessError:
        run(["cargo", "test"], cwd=rs_root, env={"HYPERION_OMEGA_REGEN_PROTO": "1"})


def main() -> int:
    before_hashes = snapshot_ch01_hashes()
    lint_and_build_descriptor()
    codegen_go()
    codegen_python()
    codegen_typescript()
    codegen_rust()
    after_hashes = snapshot_ch01_hashes()

    if before_hashes != after_hashes:
        raise RuntimeError("CRITICAL REGRESSION: Channel 01 schema/contract hashes changed during OMEGA codegen!")

    print(
        f"[omega-codegen] SUCCESS: Rust, Go, Python, & TypeScript OMEGA SDKs compiled & tested cleanly "
        f"({len(after_hashes)} Channel 01 artifacts verified 0-byte SHA-256 unchanged)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
