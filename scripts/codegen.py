#!/usr/bin/env python3
"""HYPERION deterministic multi-language Protobuf & gRPC code generator (Rust, Go, Python, TS)."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROTO_DIR = ROOT / "proto"
PROTO_FILES = [
    "common.proto",
    "orderbook.proto",
    "telemetry_fraud.proto",
    "chronos.proto",
    "stream_analytics.proto",
]


def run(cmd: list[str], cwd: Path = ROOT, env: dict[str, str] | None = None) -> None:
    print(f"[codegen] ({cwd.relative_to(ROOT) if cwd != ROOT else '.'}) $ {' '.join(cmd)}")
    merged_env = os.environ.copy()
    if env:
        merged_env.update(env)
    subprocess.run(cmd, cwd=str(cwd), env=merged_env, check=True)


def codegen_descriptor() -> None:
    out_desc = ROOT / "contracts" / "hyperion_descriptor.binpb"
    run(["buf", "build", str(PROTO_DIR), "-o", str(out_desc)])


def codegen_go() -> None:
    go_root = ROOT / "contracts" / "gen" / "go"
    go_out = go_root / "hyperionv1"
    if go_out.exists():
        shutil.rmtree(go_out)
    go_out.mkdir(parents=True, exist_ok=True)

    run(
        [
            "protoc",
            f"-I={PROTO_DIR}",
            f"--go_out={go_out}",
            "--go_opt=paths=source_relative",
            f"--go-grpc_out={go_out}",
            "--go-grpc_opt=paths=source_relative",
            *PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )
    run(["go", "mod", "tidy"], cwd=go_root)


def codegen_python() -> None:
    py_pkg = ROOT / "contracts" / "gen" / "python" / "src" / "hyperion_proto"
    py_pkg.mkdir(parents=True, exist_ok=True)
    for stale in py_pkg.glob("*_pb2*"):
        stale.unlink()

    run(
        [
            sys.executable,
            "-m",
            "grpc_tools.protoc",
            f"-I={PROTO_DIR}",
            f"--python_out={py_pkg}",
            f"--pyi_out={py_pkg}",
            f"--grpc_python_out={py_pkg}",
            *PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )

    # Rewrite intra-package bare imports (`import common_pb2 as ...`) to package-relative imports.
    module_names = "|".join(Path(p).stem + "_pb2" for p in PROTO_FILES)
    pattern = re.compile(rf"^import ({module_names}) as ([A-Za-z0-9_]+)$", re.MULTILINE)
    for generated in list(py_pkg.glob("*_pb2.py")) + list(py_pkg.glob("*_pb2_grpc.py")) + list(py_pkg.glob("*_pb2.pyi")):
        text = generated.read_text(encoding="utf-8")
        patched = pattern.sub(r"from . import \1 as \2", text)
        if patched != text:
            generated.write_text(patched, encoding="utf-8")


def codegen_typescript() -> None:
    ts_root = ROOT / "contracts" / "gen" / "ts"
    npm_cmd = "npm.cmd" if os.name == "nt" else "npm"
    if not (ts_root / "node_modules" / "ts-proto").exists():
        run([npm_cmd, "install", "--no-audit", "--no-fund"], cwd=ts_root)

    ts_gen = ts_root / "src" / "generated"
    if ts_gen.exists():
        shutil.rmtree(ts_gen)
    ts_gen.mkdir(parents=True, exist_ok=True)

    plugin_name = "protoc-gen-ts_proto.cmd" if os.name == "nt" else "protoc-gen-ts_proto"
    plugin_path = ts_root / "node_modules" / ".bin" / plugin_name

    run(
        [
            "protoc",
            f"--plugin=protoc-gen-ts_proto={plugin_path}",
            f"-I={PROTO_DIR}",
            f"--ts_proto_out={ts_gen}",
            "--ts_proto_opt=forceLong=bigint,esModuleInterop=true,outputServices=generic-definitions,exportCommonSymbols=false,env=both",
            *PROTO_FILES,
        ],
        cwd=PROTO_DIR,
    )
    run([npm_cmd, "run", "build"], cwd=ts_root)


def codegen_rust() -> None:
    rs_root = ROOT / "contracts" / "gen" / "rust"
    run(["cargo", "build"], cwd=rs_root, env={"HYPERION_REGEN_PROTO": "1"})


def main() -> int:
    codegen_descriptor()
    codegen_go()
    codegen_python()
    codegen_typescript()
    codegen_rust()
    print("[codegen] SUCCESS: Rust, Go, Python, and TypeScript contracts generated cleanly.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
