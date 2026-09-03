#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
deb_path=${1:-}

shopt -s nocasematch
forbidden_path_re='(^|/)(profiles-private|references?|screenshots?)(/|$)|(^|/).*screenshot[^/]*\.png$|(^|/).*\.(terminal|plist|tar|tar\.[a-z0-9.]+|tgz|zip|7z|xz|gz)$'
while IFS= read -r -d '' path; do
  if [[ "$path" =~ $forbidden_path_re ]]; then
    printf 'forbidden private/reference path is staged or publishable: %s\n' "$path" >&2
    exit 1
  fi
done < <(git -C "$repo_root" ls-files --cached --others --exclude-standard -z)

if [[ -n "$deb_path" ]]; then
  contents=$(dpkg-deb --contents "$deb_path")
  if grep -Eiq 'profiles-private|reference|screenshot|\.terminal$' <<<"$contents"; then
    printf 'private/reference input leaked into package: %s\n' "$deb_path" >&2
    exit 1
  fi
  extraction_root=$(mktemp -d)
  trap 'rm -rf -- "$extraction_root"' EXIT
  dpkg-deb --extract "$deb_path" "$extraction_root"
  if find "$extraction_root" -type f -exec strings {} + \
    | grep -Eq '/home/[^/]+/|/Users/[^/]+/'; then
    printf 'absolute developer home path leaked into package: %s\n' "$deb_path" >&2
    exit 1
  fi
fi

printf '%s\n' 'private and reference data checks passed'
