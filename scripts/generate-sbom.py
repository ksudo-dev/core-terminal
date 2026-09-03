#!/usr/bin/env python3
"""Generate a deterministic CycloneDX SBOM from Cargo metadata and Cargo.lock."""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
import urllib.parse
from pathlib import Path


def purl(name: str, version: str) -> str:
    encoded_name = urllib.parse.quote(name, safe="")
    encoded_version = urllib.parse.quote(version, safe="")
    return f"pkg:cargo/{encoded_name}@{encoded_version}"


def license_entry(value: str | None) -> list[dict[str, str]]:
    return [{"expression": value}] if value else []


def main() -> int:
    if len(sys.argv) != 3:
        print("usage: generate-sbom.py REPOSITORY OUTPUT", file=sys.stderr)
        return 2

    repository = Path(sys.argv[1]).resolve()
    output = Path(sys.argv[2])
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=repository,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    lock = tomllib.loads((repository / "Cargo.lock").read_text(encoding="utf-8"))
    checksums = {
        (package["name"], package["version"], package.get("source")): package.get("checksum")
        for package in lock["package"]
    }

    root_id = metadata["resolve"]["root"]
    packages = {package["id"]: package for package in metadata["packages"]}
    root = packages[root_id]
    components = []
    for package in sorted(metadata["packages"], key=lambda item: item["id"]):
        if package["id"] == root_id:
            continue
        component: dict[str, object] = {
            "type": "library",
            "bom-ref": package["id"],
            "name": package["name"],
            "version": package["version"],
            "purl": purl(package["name"], package["version"]),
        }
        licenses = license_entry(package.get("license"))
        if licenses:
            component["licenses"] = licenses
        checksum = checksums.get(
            (package["name"], package["version"], package.get("source"))
        )
        if checksum:
            component["hashes"] = [{"alg": "SHA-256", "content": checksum}]
        components.append(component)

    dependencies = [
        {"ref": node["id"], "dependsOn": sorted(node["dependencies"])}
        for node in sorted(metadata["resolve"]["nodes"], key=lambda item: item["id"])
    ]
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": root_id,
                "name": root["name"],
                "version": root["version"],
                "purl": purl(root["name"], root["version"]),
                "licenses": license_entry(root.get("license")),
            }
        },
        "components": components,
        "dependencies": dependencies,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
