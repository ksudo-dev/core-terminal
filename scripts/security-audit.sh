#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
deb_path=${1:-}
credential_pattern='AKIA[A-Z0-9]{16}|ASIA[A-Z0-9]{16}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|xox[baprs]-[A-Za-z0-9-]{20,}|-----BEGIN (RSA |EC |OPENSSH )?PRIVATE KEY-----'

while IFS= read -r -d '' path; do
  [[ "$path" == "scripts/security-audit.sh" ]] && continue
  full_path="$repo_root/$path"
  [[ -f "$full_path" ]] || continue
  if LC_ALL=C grep -Iq . "$full_path" \
    && LC_ALL=C grep -En "$credential_pattern" "$full_path"; then
    printf 'credential-shaped value found in publishable file: %s\n' "$path" >&2
    exit 1
  fi
done < <(git -C "$repo_root" ls-files --cached --others --exclude-standard -z)

"$repo_root/scripts/check-private-data.sh" ${deb_path:+"$deb_path"}

if [[ -n "$deb_path" ]]; then
  extraction_root=$(mktemp -d)
  trap 'rm -rf -- "$extraction_root"' EXIT
  dpkg-deb --extract "$deb_path" "$extraction_root"
  binary="$extraction_root/usr/bin/core-terminal"
  [[ -x "$binary" ]] || {
    printf 'package executable is missing: %s\n' "$binary" >&2
    exit 1
  }
  if readelf -d "$binary" | grep -Eq '\((RPATH|RUNPATH)\)'; then
    printf '%s\n' 'packaged executable contains RPATH or RUNPATH' >&2
    exit 1
  fi
  if ldd "$binary" | grep -Fq 'not found'; then
    printf '%s\n' 'packaged executable has an unresolved shared library' >&2
    exit 1
  fi
  if find "$extraction_root" -type f -exec strings {} + \
    | grep -Eiq 'profiles-private|/home/[^/]+/|/Users/[^/]+/'; then
    printf '%s\n' 'package contains private metadata or a developer home path' >&2
    exit 1
  fi
fi

printf '%s\n' 'security audit passed'
