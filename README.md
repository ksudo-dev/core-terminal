# Core Terminal

Core Terminal is a GTK4 terminal emulator for Linux with Terminal.app-style
profiles and reusable window groups. Profiles save how a terminal looks and
behaves. Window groups reopen an ordered set of tabs with each tab's profile,
directory, and size.

It uses VTE for PTYs, terminal protocol parsing, rendering, scrollback,
selection, and clipboard integration. Core Terminal is independent software;
it does not ship Apple source, fonts, icons, profiles, or branding.

## Install

Download the latest Debian package, Flatpak bundle, source archive, SBOM, and
checksums from the [GitHub Releases page](https://github.com/ksudo-dev/core-terminal/releases/latest).
Verify the checksums and GitHub build provenance using
[`docs/RELEASE_VERIFICATION.md`](docs/RELEASE_VERIFICATION.md) before installing
a downloaded artifact.

The Debian package targets Ubuntu 26.04 and `amd64`:

```sh
sudo apt install ./core-terminal_0.2.1_amd64.deb
```

The published Flatpak bundle targets x86_64 Linux distributions with Flatpak
support. It is a release bundle rather than a Flathub listing:

```sh
flatpak install --user ./io.github.ksudo_dev.CoreTerminal.flatpak
flatpak run io.github.ksudo_dev.CoreTerminal
```

The Flatpak uses the GNOME runtime for GTK and VTE, but does not require the
GNOME desktop. Because a terminal must run the user's host shell and commands,
it has home-directory access and uses Flatpak's host-command interface.
Its host-process supervisor requires Linux 5.3 or newer for pidfds. On an older
kernel, Core Terminal rejects the Flatpak shell launch instead of falling back
to numeric PID signaling.

## Native build dependencies

On Ubuntu 26.04, install the packages used to build and inspect the native
application and Debian package:

```sh
sudo apt install --no-install-recommends -y \
  cargo rustc rustfmt rust-clippy build-essential pkg-config \
  libgtk-4-dev libvte-2.91-gtk4-dev \
  dpkg-dev fakeroot lintian desktop-file-utils appstream
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

## Features in 0.2.1

Core Terminal currently includes:

- Profiles for appearance, fonts, colors, shell behavior, keyboard mappings,
  tabs, titles, scrollback, dimensions, and terminal behavior
- Built-in profiles plus custom profile creation, duplication, deletion, reset,
  import, and export
- Window groups that reopen ordered tabs with saved profiles, directories, and
  terminal dimensions
- VTE-backed PTY tabs and windows with login-shell startup and an optional
  custom command
- Search, selection-aware clipboard actions, tab navigation, and Ctrl+1 through
  Ctrl+9 tab switching
- XML and binary plist import for supported `.terminal` fields, with
  deterministic export and clear fallback reporting for unsupported fields
- Visual bells, background notifications, exit notifications, and tab activity
  indicators where the desktop provides the required integration
- JSON persistence under the user's XDG configuration directory

The settings window is non-modal. GTK and the compositor own window controls,
focus, placement, and pointer behavior outside the application window.

## Linux limits

Core Terminal follows the useful parts of Terminal.app's profile model, but it
is not a macOS port. Settings that depend on macOS, GTK, VTE, or Wayland are
marked unavailable with an explanation. GNOME chooses top-level window
placement in the tested Ubuntu session; other Linux compositors make the same
decision on their desktops. Window groups retain ordered profile, directory,
size, and tab data, but they cannot request macOS screen positions.

VTE owns terminal protocol modes. UTF-8 is the runtime encoding in this
release. Legacy encoding entries shown in the Encodings page are unavailable,
not hidden working controls. Blur, urgency, notifications, and Dock counters
depend on the desktop and are treated as optional Linux integrations.


## Debian package

Build and inspect a local package. The script defaults to version 0.2.1:

```sh
scripts/build-deb.sh
scripts/check-deb.sh dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
lintian --pedantic dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
```

Remove it with administrator permission:

```sh
sudo apt remove core-terminal
```

The package contains the release binary, desktop entry, ten project-owned
default profiles, icons, license and dependency notices, a Debian changelog,
and the `core-terminal(1)` manual page. The build copies an explicit allowlist.
`profiles-private/`, screenshots, `.terminal` files, and reference archives do
not enter the package.

The release `.deb` targets Ubuntu 26.04 on `amd64`. CI also installs and links
the same artifact on Debian 13. Older Debian and Ubuntu releases may not have
the required GTK, VTE, or `t64` GLib packages.

## Flatpak build

The Flatpak is the cross-distribution build. It can run under GNOME, KDE
Plasma, Cinnamon, XFCE, and other Wayland or X11 desktops with Flatpak support.

To build the bundle locally, install `flatpak` and `flatpak-builder`, add the
Flathub remote for the current user, and run:

```sh
flatpak remote-add --if-not-exists --user flathub \
  https://flathub.org/repo/flathub.flatpakrepo
scripts/check-flatpak-source.sh
scripts/build-flatpak.sh
```

The build downloads the GNOME 50 SDK and runtime on first use. Cargo crates are
resolved offline from checksummed entries generated from `Cargo.lock`.

See [`docs/TESTING.md`](docs/TESTING.md) for the release checklist and
[`docs/PARITY_MATRIX.md`](docs/PARITY_MATRIX.md) for control-by-control scope.

## User files

User settings and profile overrides are written to
`$XDG_CONFIG_HOME/core-terminal/`, or `$HOME/.config/core-terminal/` when
`XDG_CONFIG_HOME` is unset. The JSON files are written with mode `0600` on Unix
systems. Packaged builds read project defaults from their data directory:
`/usr/share/core-terminal/default-profiles.json` for the Debian package and
`/app/share/core-terminal/default-profiles.json` in Flatpak.

## License

Core Terminal is licensed under GPL-3.0-or-later. Dependency licenses and
resolved Cargo versions are listed in
[`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md).
Release downloads also include a CycloneDX dependency SBOM, SHA-256 checksums,
and GitHub build-provenance attestations.
