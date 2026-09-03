# Testing Core Terminal

Run the release checks from the repository root on Ubuntu:

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --no-fail-fast
cargo audit
bash -n scripts/*.sh
desktop-file-validate packaging/core-terminal.desktop
appstreamcli validate --no-net \
  packaging/io.github.ksudo_dev.CoreTerminal.metainfo.xml
scripts/generate-notices.sh /tmp/core-terminal-notices.md
diff -u THIRD_PARTY_NOTICES.md /tmp/core-terminal-notices.md
scripts/check-private-data.sh
scripts/check-doc-style.sh
scripts/security-audit.sh
scripts/check-flatpak-source.sh
```

## Native Wayland acceptance

The script starts the binary in an isolated D-Bus session, so it can run while
another Core Terminal process is open without forwarding activation to it.

```sh
cargo build --locked --release
scripts/native-acceptance.sh target/release/core-terminal
```

The harness runs on the current Wayland socket with GTK critical messages made
fatal. It creates an isolated XDG configuration directory, opens all four
settings pages and all six profile pages, and checks the rendered settings
allocation. The profile sidebar, names, labeled actions, and six profile tabs
must remain visible without a horizontally scrolling outer page. The harness
also edits a profile through the real Save button, reloads the JSON, checks
applied VTE properties, creates and closes a tab, verifies non-modal settings,
and confirms pointer autohide is disabled. The script prints one `status=PASS`
line and removes the temporary configuration directory when it exits.

The harness cannot control GL.iNet Comet's browser Pointer Lock. A maintainer
using that KVM must also take a macOS and GNOME screenshot, return focus to Core
Terminal, and click a settings tab, Cancel, and Save. The app never requests
Pointer Lock and never asks VTE to hide its pointer.

## Private profile fixtures

The ten supplied `.terminal` files live outside the repository. Run their
ignored parser test only when that private directory is available:

```sh
CORE_TERMINAL_PRIVATE_FIXTURE_DIR=/absolute/path/to/profiles-private \
  cargo test --locked \
  profiles::tests::private_reference_profiles_decode_when_explicitly_requested \
  -- --ignored --exact
```

The environment variable is opt-in. Its path and files must stay outside Git
and the Debian package.

## Debian package

```sh
scripts/build-deb.sh 0.2.0
deb=dist/core-terminal_0.2.0_$(dpkg --print-architecture).deb
scripts/check-deb.sh "$deb"
scripts/check-private-data.sh "$deb"
scripts/security-audit.sh "$deb"
lintian --pedantic "$deb"
```

Before publishing, extract the package into a temporary directory and inspect
the executable:

```sh
package_root=$(mktemp -d)
dpkg-deb --extract "$deb" "$package_root"
readelf -d "$package_root/usr/bin/core-terminal"
ldd "$package_root/usr/bin/core-terminal"
```

Reject a package with `RPATH`, `RUNPATH`, a missing shared library, a developer
home path, a screenshot, a `.terminal` file, or `profiles-private/` content.

CI installs the generated `.deb` in a clean Debian 13 container and rejects an
unresolved dependency or shared library. This is an installation check, not a
claim that the package works on Debian 12 or every Debian-derived release.

## Flatpak bundle

Add the Flathub user remote once, then build the cross-distribution bundle:

```sh
flatpak remote-add --if-not-exists --user flathub \
  https://flathub.org/repo/flathub.flatpakrepo
scripts/check-flatpak-source.sh
scripts/build-flatpak.sh
flatpak install --user --or-update \
  dist/io.github.ksudo_dev.CoreTerminal.flatpak
flatpak info --user io.github.ksudo_dev.CoreTerminal
flatpak run --user --command=flatpak-spawn \
  io.github.ksudo_dev.CoreTerminal \
  --host --clear-env --env=HOME="$HOME" --directory="$HOME" \
  /bin/sh -c 'test "$PWD" = "$HOME"'
```

The last command checks the host-shell bridge without starting the graphical
application. CI also launches the installed Flatpak under X11 and runs the
real GTK/VTE acceptance harness. A native desktop acceptance run remains
required for GTK, VTE, Wayland, tabs, settings, and pointer behavior.
