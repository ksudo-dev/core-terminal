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
grep -Fqx "        url: https://download.gnome.org/sources/vte/0.84/vte-0.84.1.tar.xz" "$manifest"
grep -Fqx "        sha256: aca1caa8478aebcdbb1d67897fb3511eb7601debae6810e16a15b6fa25f31ac8" "$manifest"
grep -Fqx "      - -Dgtk4=true" "$manifest"
grep -Fqx "      - -Dgtk3=false" "$manifest"
grep -Fqx "      - cc -std=c11 -O2 -D_FORTIFY_SOURCE=3 -Wall -Wextra -Werror -fstack-protector-strong -fPIE -static-pie -Wl,-z,relro,-z,now src/flatpak-host-supervisor.c -o core-terminal-host-supervisor" "$manifest"
grep -Fqx "      - install -Dm0755 core-terminal-host-supervisor \${FLATPAK_DEST}/libexec/core-terminal-host-supervisor" "$manifest"
grep -Fqx "      - install -m0644 ../COPYING.CC-BY-4-0 ../COPYING.GPL3 ../COPYING.LGPL3 ../COPYING.README ../COPYING.XTERM \${FLATPAK_DEST}/share/doc/core-terminal/vte/" "$manifest"
grep -Fqx "StartupWMClass=$app_id" "$repo_root/packaging/core-terminal.desktop"
grep -Fqx "Icon=$app_id" "$repo_root/packaging/core-terminal.desktop"

if grep -Eq -- '--filesystem=(host|host-os|host-etc)|--socket=(session-bus|system-bus)|--share=network' "$manifest"; then
  echo "Flatpak manifest requests an unapproved broad permission" >&2
  exit 1
fi

printf '%s\n' 'Flatpak source manifest and metadata checks passed'
