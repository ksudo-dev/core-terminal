#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
binary=${1:-"$repo_root/target/release/core-terminal"}

if [[ ! -x "$binary" ]]; then
  printf 'acceptance binary is not executable: %s\n' "$binary" >&2
  exit 1
fi
if [[ -z "${WAYLAND_DISPLAY:-}" || -z "${XDG_RUNTIME_DIR:-}" ]]; then
  printf '%s\n' 'native Wayland environment is unavailable' >&2
  exit 1
fi
if [[ ! -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]]; then
  printf 'Wayland socket does not exist: %s\n' \
    "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" >&2
  exit 1
fi
if ! command -v dbus-run-session >/dev/null; then
  printf '%s\n' 'dbus-run-session is required for isolated acceptance' >&2
  exit 1
fi

work_root=$(mktemp -d /tmp/core-terminal-acceptance.XXXXXX)
trap 'rm -rf -- "$work_root"' EXIT
report="$work_root/report.txt"
install -d -m 0700 "$work_root/config"

(
  cd "$repo_root"
  dbus-run-session -- env \
    GDK_BACKEND=wayland \
    G_DEBUG=fatal-criticals \
    XDG_CONFIG_HOME="$work_root/config" \
    CORE_TERMINAL_ACCEPTANCE=1 \
    CORE_TERMINAL_ACCEPTANCE_REPORT="$report" \
      "$binary"
)

if [[ ! -f "$report" ]]; then
  printf '%s\n' 'acceptance report was not written' >&2
  exit 1
fi
cat "$report"
grep -Fq 'status=PASS ' "$report"
