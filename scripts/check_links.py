#!/usr/bin/env python3
"""HYPERION Documentation Integrity & Zero-Broken-Link Verifier."""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")


def check_markdown_files() -> list[tuple[Path, int, str]]:
    targets: list[Path] = []
    readme = REPO_ROOT / "README.md"
    if readme.exists():
        targets.append(readme)
    ignored_dirs = {"node_modules", ".venv", "venv", "target", ".next", ".git", "dist"}
    for folder in ["docs", "specs/handoffs", "contracts"]:
        p = REPO_ROOT / folder
        if p.exists():
            for md in sorted(p.rglob("*.md")):
                if any(part in ignored_dirs for part in md.relative_to(REPO_ROOT).parts):
                    continue
                targets.append(md)

    broken: list[tuple[Path, int, str]] = []
    for md in targets:
        text = md.read_text(encoding="utf-8", errors="replace")
        in_fence = False
        for lineno, line in enumerate(text.splitlines(), start=1):
            stripped = line.strip()
            if stripped.startswith("```"):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            for match in LINK_RE.finditer(line):
                raw_target = match.group(1).strip()
                if not raw_target or raw_target.startswith(("http://", "https://", "mailto:", "#")):
                    continue
                target_path_str = raw_target.split("#", 1)[0].split("?", 1)[0].strip()
                if not target_path_str:
                    continue
                if target_path_str.startswith("file:///"):
                    resolved = Path(target_path_str[len("file:///"):])
                else:
                    resolved = (md.parent / target_path_str).resolve()
                if not resolved.exists():
                    broken.append((md.relative_to(REPO_ROOT), lineno, raw_target))
    return broken


def main() -> int:
    broken = check_markdown_files()
    if broken:
        print(f"BROKEN LINKS DETECTED ({len(broken)}):", file=sys.stderr)
        for rel, line, target in broken:
            print(f"  {rel}:{line} -> {target}", file=sys.stderr)
        return 1
    print("OK: 0 broken documentation links across README.md, docs/, specs/handoffs/, contracts/.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
