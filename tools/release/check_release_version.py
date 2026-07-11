#!/usr/bin/env python3
"""Validate that a release tag matches every user-facing package version."""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path


MANIFESTS = (
    Path("Cargo.toml"),
    Path("firmware/Cargo.toml"),
    Path("tools/hidshift-client/Cargo.toml"),
    Path("tools/hidshiftctl/Cargo.toml"),
    Path("web/Cargo.toml"),
)
VERSION_RE = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$")


def validate(root: Path, tag: str) -> list[str]:
    errors: list[str] = []
    if not tag.startswith("v") or not VERSION_RE.fullmatch(tag[1:]):
        return [f"tag {tag!r} must have the form vX.Y.Z or vX.Y.Z-prerelease"]

    release_version = tag[1:]
    package_version = release_version.split("-", 1)[0]
    for relative_path in MANIFESTS:
        path = root / relative_path
        with path.open("rb") as manifest_file:
            actual = tomllib.load(manifest_file)["package"]["version"]
        if actual != package_version:
            errors.append(f"{relative_path}: expected {package_version}, found {actual}")

    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    if not re.search(rf"^## {re.escape(package_version)}(?:\s+-|$)", changelog, re.MULTILINE):
        errors.append(f"CHANGELOG.md: missing heading for {package_version}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("tag", help="Git tag, for example v0.1.0")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()
    errors = validate(args.root, args.tag)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"release version {args.tag} is consistent")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
