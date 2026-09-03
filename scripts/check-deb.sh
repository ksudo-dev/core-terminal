#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
deb_path=${1:?usage: scripts/check-deb.sh path/to/package.deb}

dpkg-deb --info "$deb_path"
dpkg-deb --contents "$deb_path"

contents=$(dpkg-deb --contents "$deb_path")
required_paths=(
  "usr/bin/core-terminal" \
  "usr/share/applications/io.github.ksudo_dev.CoreTerminal.desktop" \
  "usr/share/metainfo/io.github.ksudo_dev.CoreTerminal.metainfo.xml" \
  "usr/share/core-terminal/default-profiles.json" \
  "usr/share/doc/core-terminal/LICENSE" \
  "usr/share/doc/core-terminal/copyright" \
  "usr/share/doc/core-terminal/THIRD_PARTY_NOTICES.md" \
  "usr/share/doc/core-terminal/changelog.gz" \
  "usr/share/lintian/overrides/core-terminal" \
  "usr/share/man/man1/core-terminal.1.gz" \
  "usr/share/icons/hicolor/32x32/apps/io.github.ksudo_dev.CoreTerminal.png" \
  "usr/share/icons/hicolor/64x64/apps/io.github.ksudo_dev.CoreTerminal.png" \
  "usr/share/icons/hicolor/128x128/apps/io.github.ksudo_dev.CoreTerminal.png" \
  "usr/share/icons/hicolor/256x256/apps/io.github.ksudo_dev.CoreTerminal.png" \
  "usr/share/icons/hicolor/512x512/apps/io.github.ksudo_dev.CoreTerminal.png"
)
for required in "${required_paths[@]}"; do
  grep -Fq "./$required" <<<"$contents" || {
    echo "missing package path: $required" >&2
    exit 1
  }
done

extraction_root=$(mktemp -d)
trap 'rm -rf -- "$extraction_root"' EXIT
dpkg-deb --extract "$deb_path" "$extraction_root"
declare -A allowed=()
for required in "${required_paths[@]}"; do
  allowed["$required"]=1
done
while IFS= read -r -d '' installed; do
  relative=${installed#"$extraction_root/"}
  if [[ -z "${allowed[$relative]:-}" ]]; then
    echo "unexpected package path: $relative" >&2
    exit 1
  fi
done < <(find "$extraction_root" \( -type f -o -type l \) -print0)

desktop-file-validate "$repo_root/packaging/core-terminal.desktop"
appstreamcli validate --no-net \
  "$repo_root/packaging/io.github.ksudo_dev.CoreTerminal.metainfo.xml"
grep -Fqx "Icon=io.github.ksudo_dev.CoreTerminal" \
  "$extraction_root/usr/share/applications/io.github.ksudo_dev.CoreTerminal.desktop"

if grep -Eiq "profiles-private|reference|screenshot|\.terminal$" <<<"$contents"; then
  echo "private reference metadata leaked into package" >&2
  exit 1
fi

echo "package contents match the allowlist and contain no private reference metadata"
