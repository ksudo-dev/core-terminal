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
scripts/check-flatpak-host-supervisor.py
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
must remain visible without a horizontally scrolling outer page. Shell inputs
must also expose GTK accessibility labels and descriptions. The harness
requires the Text form itself to be attached and visible, poisons the legacy
global snapshot, loads the active profile's font, cursor, scrollback, and TERM
values, switches to another profile, then saves. It reloads the JSON and proves
that editable values changed, unavailable imported values survived, and the
compatibility snapshot still describes the active profile. It also checks
applied VTE properties, creates and closes a tab, verifies non-modal settings,
confirms pointer autohide is disabled, and exercises tab-close and shell-exit
window confirmation with a protected sibling. It also closes a genuinely
pending VTE spawn before its callback returns, checks the callback cleanup,
revalidates stale confirmations, and preserves a queued window-close request.
It also captures a live shell session containing foreground and background
jobs, verifies their distinct process-group roles and per-run process names,
accepts any safely revalidated close prompt, and verifies that every captured
process exits. Emergency probe cleanup also opens a pidfd before checking PID,
start time, session, and process group, then sends the signal through that
kernel-bound handle.

The script prints one `status=PASS` line and removes the temporary configuration
directory when it exits.

The Flatpak CI run keeps the foreground/background probe but splits observation
at the sandbox boundary. The app verifies the exact local proxy token, its
`--host`, `--watch-bus`, and `--forward-fd=3` arguments, and the fixed
`/proc/self/fd/3` executable. A host-side watcher binds pidfds to both exact
marked job argument vectors, verifies their controlling-terminal foreground
and background process-group roles, acknowledges that they are live before the
app closes the tab, and then requires both pidfds to report exit. This prevents
a sandbox-only process scan from producing a false pass.

`scripts/check-flatpak-host-supervisor.py` compiles the C pidfd helper as a
static PIE with warnings treated as errors. It gives the helper an already-open
but unclaimed pseudoterminal, then requires the helper to acquire and verify it
as its controlling terminal. The suite verifies cleanup of distinct foreground
and background process groups after HUP, INT, QUIT, TERM, USR1, and USR2. It
also accepts a PTY already owned by the helper's session, rejects a PTY owned by
another session without starting the payload, and checks the host shell owns
the terminal's foreground process group. Residual-job cleanup and exit-status
preservation are tested when the direct child exits normally. A low-descriptor
test sets `RLIMIT_NOFILE` to 8, starts 40 signal-ignoring jobs, and proves that
streaming pidfd cleanup does not depend on holding one descriptor per session
member. A real PTY interaction test changes the terminal size and observes
`SIGWINCH`, suspends a foreground job with Ctrl-Z, resumes the same bound
process with `fg`, interrupts it with Ctrl-C, and requires Bash status 130.

The harness cannot control GL.iNet Comet's browser Pointer Lock. A maintainer
using that KVM must also take a macOS and GNOME screenshot, return focus to Core
Terminal, and click a settings tab, Cancel, and Save. The app never requests
Pointer Lock and never asks VTE to hide its pointer.

The harness automates the background-session and close-race cases. These manual
native checks remain useful for release candidates because a Flatpak cannot
inspect the host PTY's foreground process group:

1. Start a foreground command whose name is in the profile exception list and
   confirm that closing its tab does not prompt.
2. Start a non-exempt foreground command such as `sleep 60` and confirm that
   closing its tab asks before terminating it.
3. Run `sleep 60 &` from an otherwise idle login shell and confirm that the
   background job still causes a close prompt.
4. Set shell-exit behavior to close the window, keep a protected command open
   in a second tab, and exit the first shell. The window must ask before closing
   the protected sibling.
5. While a confirmation is open, change which process is running. Confirming
   the stale prompt must not close the new process without another check.
6. Request a whole-window close while a tab confirmation is open. Cancelling
   or accepting the tab prompt must not discard the pending window request.
7. Close a tab immediately after opening it and confirm that a child returned
   by the asynchronous spawn callback is terminated instead of orphaned.

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
scripts/build-deb.sh 0.2.1
deb=dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
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
expected_host_shell=$(getent passwd "$(id -u)" | cut -d: -f7)
flatpak run --user --command=flatpak-spawn \
  io.github.ksudo_dev.CoreTerminal \
  --host --directory="$HOME" \
  "$PWD/scripts/check-flatpak-host-environment.sh" \
  "$expected_host_shell" "$HOME"
```

The last command checks the host-shell bridge without replacing the host
session environment. CI also launches the installed Flatpak under X11 and
runs the real GTK/VTE acceptance harness. A native desktop acceptance run
remains required for GTK, VTE, Wayland, tabs, settings, and pointer behavior.
