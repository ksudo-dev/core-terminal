# Core Terminal

Core Terminal is a GTK4 terminal emulator for Linux. VTE provides the PTY,
terminal protocol parsing, rendering, scrollback, selection, and clipboard
integration. The application ID is `app.coreterminal.CoreTerminal`.

The settings layout takes Terminal.app as a visual reference. Core Terminal is
independent software. It does not ship Apple source, fonts, icons, profiles, or
branding.

## Build dependencies

On Ubuntu or another Debian-based distribution, install the packages used to
build and inspect the application:

```sh
sudo apt install --no-install-recommends -y \
  cargo rustc rustfmt rust-clippy build-essential pkg-config \
  libgtk-4-dev libvte-2.91-gtk4-dev \
  dpkg-dev fakeroot lintian desktop-file-utils
```

The GTK4 and VTE development packages must provide the APIs selected in
`Cargo.toml`. The native launch test requires an active GNOME Wayland session.

## Build and test

Run these commands from the repository root:

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --no-fail-fast
cargo build --locked --release
```

For a native Wayland launch, run the release binary from the Ubuntu desktop:

```sh
test -n "${WAYLAND_DISPLAY:-}" || exit 1
GDK_BACKEND=wayland cargo run --locked --release
```

The native acceptance harness checks the real settings widget tree, profile
persistence, VTE property application, tab lifecycle, and pointer policy:

```sh
scripts/native-acceptance.sh target/release/core-terminal
```

The script uses an isolated D-Bus session so an installed Core Terminal process
cannot intercept the test activation. Weston is not a substitute for the
Ubuntu GNOME Wayland test.

## Features in 0.2.0

Core Terminal currently includes:

- VTE-backed PTY tabs and windows
- login-shell startup and an optional custom command
- profile selection, built-in profiles, custom profile duplication and removal
- XML and binary plist import for the supported `.terminal` fields
- deterministic export of the supported profile fields
- a non-modal settings window with General, Profiles, Window Groups, and
  Encodings pages
- profile pages for Text, Window, Tab, Shell, Keyboard, and Advanced settings
- a fixed profile sidebar with readable names, labeled profile actions, and no
  horizontal wheel path that can slide the editor out of view
- profile colors, ANSI palette, font, cursor, scrollback, titles, shell exit
  policy, keyboard mappings, terminal type, locale, character width, text
  blinking, and dimensions
- selection-aware Ctrl+C and Ctrl+V handling, Option/Alt Meta input, Ctrl-H
  Delete behavior, non-ASCII escaping, newline-to-carriage-return paste, and
  editable F1 through F12 mappings
- saved window groups that can be selected at startup and launch ordered tabs
  with their saved profile, directory, and terminal dimensions
- visual bells, background notifications, exit notifications, and tab activity
  indicators where GNOME exposes the required API
- search, clipboard actions, tab navigation, and Ctrl+1 through Ctrl+9 tab
  switching
- JSON persistence under the user's XDG configuration directory
- a visible pointer policy that keeps VTE pointer autohide disabled for KVM and
  screenshot focus changes

The settings window is non-modal. GTK and the compositor own window controls,
focus, placement, and pointer behavior outside the application window.

## Linux limits

Every setting visible in the reference screenshots has a corresponding control
or an unavailable control with a reason. Core Terminal does not claim exact
macOS behavior for Dock tile contents, Dock bouncing, secure keyboard entry,
global window coordinates under Wayland, or live process restoration. GNOME
chooses top-level window placement. Window groups retain ordered profile,
directory, size, and tab data, but they cannot request macOS screen positions.

VTE owns terminal protocol modes. UTF-8 is the runtime encoding in this
release. Legacy encoding entries shown in the Encodings page are unavailable,
not hidden working controls. Blur, urgency, notifications, and Dock counters
depend on the desktop and are treated as optional Linux integrations.

The GL.iNet Comet browser banner about a hidden mouse pointer comes from
browser Pointer Lock. Core Terminal cannot control browser chrome. For KVM
testing, use Comet Absolute Mode and keep the local cursor visible. Core
Terminal does not request pointer lock or hide the VTE pointer.

## Debian package

Build and inspect a local package. The script defaults to version 0.2.0:

```sh
scripts/build-deb.sh
scripts/check-deb.sh dist/core-terminal_0.2.0_$(dpkg --print-architecture).deb
lintian --pedantic dist/core-terminal_0.2.0_$(dpkg --print-architecture).deb
```

Install and remove it with administrator permission:

```sh
sudo apt install ./dist/core-terminal_0.2.0_$(dpkg --print-architecture).deb
sudo apt remove core-terminal
```

The package contains the release binary, desktop entry, ten project-owned
default profiles, icons, license and dependency notices, a Debian changelog,
and the `core-terminal(1)` manual page. The build copies an explicit allowlist.
`profiles-private/`, screenshots, `.terminal` files, and reference archives do
not enter the package.

See [`docs/TESTING.md`](docs/TESTING.md) for the release checklist and
[`docs/PARITY_MATRIX.md`](docs/PARITY_MATRIX.md) for control-by-control scope.

## User files

User settings and profile overrides are written to
`$XDG_CONFIG_HOME/core-terminal/`, or `$HOME/.config/core-terminal/` when
`XDG_CONFIG_HOME` is unset. The JSON files are written with mode `0600` on Unix
systems. Project defaults are read from
`/usr/share/core-terminal/default-profiles.json` after installation.

## License

Core Terminal is licensed under GPL-3.0-or-later. Dependency licenses and
resolved Cargo versions are listed in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
