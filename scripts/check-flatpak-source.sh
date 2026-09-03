#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
app_id=io.github.ksudo_dev.CoreTerminal
manifest="$repo_root/packaging/${app_id}.yml"
metadata="$repo_root/packaging/${app_id}.metainfo.xml"
generated=$(mktemp)
trap 'rm -f -- "$generated"' EXIT

"$repo_root/scripts/generate-flatpak-sources.py" \
  "$repo_root/Cargo.lock" \
  "$generated"
diff -u "$repo_root/packaging/cargo-sources.json" "$generated"
python3 -m json.tool "$repo_root/packaging/cargo-sources.json" >/dev/null
desktop-file-validate "$repo_root/packaging/core-terminal.desktop"
appstreamcli validate --no-net "$metadata"

grep -Fqx "app-id: $app_id" "$manifest"
grep -Fqx "  - --filesystem=home" "$manifest"
grep -Fqx "  - --talk-name=org.freedesktop.Flatpak" "$manifest"
grep -Fqx "StartupWMClass=$app_id" "$repo_root/packaging/core-terminal.desktop"
grep -Fqx "Icon=$app_id" "$repo_root/packaging/core-terminal.desktop"

if grep -Eq -- '--filesystem=(host|host-os|host-etc)|--socket=(session-bus|system-bus)|--share=network' "$manifest"; then
  echo "Flatpak manifest requests an unapproved broad permission" >&2
  exit 1
fi

printf '%s\n' 'Flatpak source manifest and metadata checks passed'
