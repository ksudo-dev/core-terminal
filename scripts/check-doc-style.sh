#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
style_pattern='—|oaicite|contentReference|grok_card|attributableIndex|turn[0-9]+search[0-9]+|In today.s world|It.s important to note|Furthermore|Moreover|That being said|\bdelve\b|\bleverage\b|\butilize\b|\brobust\b|\bcomprehensive\b|\bseamless\b'
documents=(
  "$repo_root/README.md"
  "$repo_root/SECURITY.md"
  "$repo_root/CONTRIBUTING.md"
  "$repo_root/CHANGELOG.md"
  "$repo_root/PARKING_LOT.md"
  "$repo_root/.github/pull_request_template.md"
  "$repo_root/docs/PARITY_MATRIX.md"
  "$repo_root/docs/TESTING.md"
)

if LC_ALL=C grep -En "$style_pattern" "${documents[@]}"; then
  printf '%s\n' 'documentation contains a blocked no-ai-slop pattern' >&2
  exit 1
fi

printf '%s\n' 'documentation style check passed'
