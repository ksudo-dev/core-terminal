#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
output=${1:-"$repo_root/THIRD_PARTY_NOTICES.md"}
metadata=$(mktemp)
temporary=$(mktemp)
trap 'rm -f "$metadata" "$temporary"' EXIT

cargo metadata --locked --format-version 1 --manifest-path "$repo_root/Cargo.toml" >"$metadata"

{
  printf '%s\n' '# Third-party notices' '' \
    'This file lists every registry package resolved by Cargo for Core Terminal.' \
    'Versions and license expressions come from `cargo metadata --locked`; the' \
    'Cargo.lock file selects the resolved package versions. License text remains' \
    'in the corresponding package source cache and is not copied into this file.' '' \
    '| Package | Version | License expression | Repository |' \
    '| --- | --- | --- | --- |'
  jq -r '
    .packages[]
    | select(.source != null)
    | [ .name, .version, (.license // "License expression not supplied"), (.repository // "") ]
    | @tsv
  ' "$metadata" | while IFS=$'\t' read -r name version license repository; do
    printf '| `%s` | `%s` | `%s` | %s |\n' "$name" "$version" "$license" "$repository"
  done
} >"$temporary"

install -D -m 0644 "$temporary" "$output"
printf '%s\n' "$output"
