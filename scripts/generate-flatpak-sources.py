#!/usr/bin/env python3
"""Generate offline Flatpak Cargo sources from Cargo.lock."""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: generate-flatpak-sources.py Cargo.lock OUTPUT.json", file=sys.stderr)
        return 2

    lock_path = Path(sys.argv[1])
    output_path = Path(sys.argv[2])
    lock = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    sources: list[dict[str, object]] = []

    for package in lock["package"]:
        registry = package.get("source")
        if registry is None:
            continue
        if not registry.startswith("registry+"):
            raise ValueError(
                f"unsupported non-registry Cargo source for {package['name']}: {registry}"
            )
        checksum = package.get("checksum")
        if not checksum:
            raise ValueError(f"missing checksum for {package['name']} {package['version']}")
        name = package["name"]
        version = package["version"]
        destination = f"cargo/vendor/{name}-{version}"
        sources.extend(
            [
                {
                    "type": "archive",
                    "archive-type": "tar-gzip",
                    "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                    "sha256": checksum,
                    "dest": destination,
                },
                {
                    "type": "inline",
                    "contents": json.dumps({"package": checksum, "files": {}}),
                    "dest": destination,
                    "dest-filename": ".cargo-checksum.json",
                },
            ]
        )

    sources.append(
        {
            "type": "inline",
            "contents": (
                '[source.crates-io]\nreplace-with = "vendored-sources"\n\n'
                '[source.vendored-sources]\ndirectory = "cargo/vendor"\n'
            ),
            "dest": "cargo",
            "dest-filename": "config",
        }
    )
    output_path.write_text(json.dumps(sources, indent=4) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
