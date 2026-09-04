#!/usr/bin/env bash
set -euo pipefail

expected_shell=${1:?usage: check-flatpak-host-environment.sh SHELL HOME}
expected_home=${2:?usage: check-flatpak-host-environment.sh SHELL HOME}

test "$PWD" = "$expected_home"
test "${HOME:-}" = "$expected_home"
test "${SHELL:-}" = "$expected_shell"
test -n "${DBUS_SESSION_BUS_ADDRESS:-}"

case "$DBUS_SESSION_BUS_ADDRESS" in
  *'/run/flatpak/bus'*)
    echo 'host command received the sandbox D-Bus proxy' >&2
    exit 1
    ;;
esac

case "${XDG_CONFIG_HOME:-}" in
  *'/.var/app/io.github.ksudo_dev.CoreTerminal/'*)
    echo 'host command received the sandbox XDG config path' >&2
    exit 1
    ;;
esac

gdbus call \
  --session \
  --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus \
  --method org.freedesktop.DBus.ListNames \
  >/dev/null

printf '%s\n' 'Flatpak broker supplied a usable host session environment'
