#!/usr/bin/env python3
"""
Inspects compiled `.wasm` binary artifacts at the WebAssembly binary-format level (magic `\0asm`,
version `1`, section headers, Import section `2`, Memory section `5`, Export section `7`),
verifying Zero-WASI imports, 1 MiB (16 pages) initial memory, 16 MiB (256 pages) max memory,
and required Hyperion Quant C-ABI exports, then writes `artifacts/manifest.json`.
"""

from __future__ import annotations

import hashlib
import json
import pathlib
import sys


def read_leb128_u32(data: bytes, pos: int) -> tuple[int, int]:
    result = 0
    shift = 0
    while True:
        b = data[pos]
        pos += 1
        result |= (b & 0x7F) << shift
        if (b & 0x80) == 0:
            return result, pos
        shift += 7


def inspect_wasm_binary(wasm_bytes: bytes) -> dict:
    if len(wasm_bytes) < 8 or wasm_bytes[:4] != b"\x00asm" or wasm_bytes[4:8] != b"\x01\x00\x00\x00":
        raise ValueError("Invalid WebAssembly binary header")

    pos = 8
    imports: list[dict] = []
    exports: list[dict] = []
    memories: list[dict] = []

    while pos < len(wasm_bytes):
        sec_id = wasm_bytes[pos]
        pos += 1
        sec_len, pos = read_leb128_u32(wasm_bytes, pos)
        sec_end = pos + sec_len
        cur = pos

        if sec_id == 2:  # Import section
            count, cur = read_leb128_u32(wasm_bytes, cur)
            for _ in range(count):
                mod_len, cur = read_leb128_u32(wasm_bytes, cur)
                mod_name = wasm_bytes[cur : cur + mod_len].decode("utf-8")
                cur += mod_len
                fld_len, cur = read_leb128_u32(wasm_bytes, cur)
                fld_name = wasm_bytes[cur : cur + fld_len].decode("utf-8")
                cur += fld_len
                kind = wasm_bytes[cur]
                cur += 1
                if kind == 0:  # func
                    _, cur = read_leb128_u32(wasm_bytes, cur)
                elif kind == 1:  # table
                    cur += 1
                    flags, cur = read_leb128_u32(wasm_bytes, cur)
                    _, cur = read_leb128_u32(wasm_bytes, cur)
                    if flags & 1:
                        _, cur = read_leb128_u32(wasm_bytes, cur)
                elif kind == 2:  # mem
                    flags, cur = read_leb128_u32(wasm_bytes, cur)
                    _, cur = read_leb128_u32(wasm_bytes, cur)
                    if flags & 1:
                        _, cur = read_leb128_u32(wasm_bytes, cur)
                elif kind == 3:  # global
                    cur += 2
                imports.append({"module": mod_name, "name": fld_name, "kind": kind})

        elif sec_id == 5:  # Memory section
            count, cur = read_leb128_u32(wasm_bytes, cur)
            for _ in range(count):
                flags, cur = read_leb128_u32(wasm_bytes, cur)
                initial_pages, cur = read_leb128_u32(wasm_bytes, cur)
                max_pages = None
                if flags & 1:
                    max_pages, cur = read_leb128_u32(wasm_bytes, cur)
                memories.append(
                    {
                        "initial_pages": initial_pages,
                        "initial_bytes": initial_pages * 65536,
                        "max_pages": max_pages,
                        "max_bytes": (max_pages * 65536) if max_pages is not None else None,
                    }
                )

        elif sec_id == 7:  # Export section
            count, cur = read_leb128_u32(wasm_bytes, cur)
            for _ in range(count):
                nm_len, cur = read_leb128_u32(wasm_bytes, cur)
                nm = wasm_bytes[cur : cur + nm_len].decode("utf-8")
                cur += nm_len
                kind = wasm_bytes[cur]
                cur += 1
                idx, cur = read_leb128_u32(wasm_bytes, cur)
                exports.append({"name": nm, "kind": kind, "index": idx})

        pos = sec_end

    return {
        "size_bytes": len(wasm_bytes),
        "sha256": hashlib.sha256(wasm_bytes).hexdigest(),
        "blake2b_256": hashlib.blake2b(wasm_bytes, digest_size=32).hexdigest(),
        "imports": imports,
        "memories": memories,
        "exports": [e["name"] for e in exports],
    }


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent
    art_dir = root / "artifacts"
    required_exports = {
        "memory",
        "hyperion_abi_shm_ptr",
        "on_init",
        "on_book_l2_tick",
        "on_trade_tick",
        "on_order_fill",
        "hyperion_export_state",
        "hyperion_import_state",
    }

    manifest = {"abi_version": 1, "artifacts": {}}
    for name in [
        "stat_arb_zscore.wasm",
        "obi_sniper.wasm",
        "delta_hedger.wasm",
        "malware_bombs.wasm",
    ]:
        p = art_dir / name
        raw = p.read_bytes()
        info = inspect_wasm_binary(raw)
        exp_set = set(info["exports"])
        missing = sorted(required_exports - exp_set)
        if missing:
            raise RuntimeError(f"{name} missing required ABI exports: {missing}")
        for imp in info["imports"]:
            if imp["module"] != "hyperion" or imp["name"] not in {
                "submit_limit_order",
                "cancel_order",
            }:
                raise RuntimeError(f"{name} contains forbidden import: {imp}")
        if len(info["memories"]) != 1:
            raise RuntimeError(f"{name} expected 1 linear memory, got {info['memories']}")
        mem0 = info["memories"][0]
        if mem0["initial_pages"] != 16 or mem0["max_pages"] != 256:
            raise RuntimeError(f"{name} unexpected memory limits: {mem0}")
        manifest["artifacts"][name] = info
        print(
            f"[WASM VERIFIED] {name:<24} | {info['size_bytes']:>6} B | pages={mem0['initial_pages']}..{mem0['max_pages']} | imports={len(info['imports'])} | sha256={info['sha256'][:16]}..."
        )

    out_file = art_dir / "manifest.json"
    out_file.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(f"[WASM MANIFEST] Wrote {out_file}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
