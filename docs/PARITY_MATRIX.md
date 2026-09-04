# Core Terminal parity matrix

Audit date: 2026-09-03

This matrix compares the requested Terminal.app settings layout with Core
Terminal 0.2.1 on Ubuntu GNOME Wayland. A control counts as implemented only
when it has a GTK control, persists its value, and changes application or VTE
behavior. A value that is only stored does not count.

## Current status

| Area | Status in 0.2.1 |
| --- | --- |
| Native terminal | GTK4 application with one VTE PTY per tab, login-shell startup, and child-process cleanup |
| Windows and tabs | New window, new tab, close tab, tab navigation, and Ctrl+1 through Ctrl+9 switching |
| Settings window | Non-modal General, Profiles, Window Groups, and Encodings pages with a fixed profile sidebar and no outer horizontal scroll |
| Profile pages | Text, Window, Tab, Shell, Keyboard, and Advanced pages are scrollable and keyboard reachable |
| Profiles | Ten project-owned defaults, profile selection, add, duplicate, delete, reset, default selection, import, and export |
| Persistence | User settings and profile overrides use JSON under the XDG configuration directory and are written atomically with mode 0600 |
| Pointer behavior | VTE pointer autohide is disabled for every terminal; Core Terminal does not request browser Pointer Lock |
| Encodings | UTF-8 is active. Legacy entries are labeled unavailable because the current VTE wrapper does not expose a safe per-session selector |
| Window Groups | Logical groups can be added, removed, renamed, saved, selected for startup, and launched as ordered tabs |

The release binary passed the built-in GTK/VTE acceptance harness on the native
GNOME Wayland socket `wayland-0`. The harness opened every settings page,
activated the real Save callback, reloaded the saved profile, checked VTE
runtime properties, exercised tab creation and closure, and verified that VTE
pointer autohide remained disabled. It also checked the settings allocation,
readable profile labels, labeled profile actions, and unclipped profile tabs. A
browser-based KVM can still add a Pointer Lock layer outside this process.

## Settings coverage

### General

Implemented controls include startup profile, login shell path, optional
custom command, new-window profile, new-tab profile, same-directory behavior,
and Ctrl+1 through Ctrl+9 tab switching. The shell path must be an absolute
path. A custom command runs through the login shell when that mode is selected.

### Profiles

The editor exposes the six requested pages and seven text-labeled profile
actions. The profile list has its own vertical scroller; horizontal wheel input
cannot shift the sidebar or editor tabs out of view. Built-in names remain
protected from deletion. Import accepts bounded XML or binary plist data
and maps the supported Terminal profile fields. Export writes a deterministic
plist containing the supported Core Terminal fields. Unknown or unsupported
Apple fields are not executed and may be reported as fallbacks.

Profile operations covered by unit or native acceptance tests are:

1. Add or duplicate a profile.
2. Edit it on each page.
3. Save, restart, and compare the saved values.
4. Delete the custom profile while retaining all built-ins.
5. Import a plist, then export and re-import it.

### Text

The page exposes font family and size, foreground, background, bold, selection,
and cursor colors, opacity, antialiasing, bold-font use, text blinking, ANSI
color use, bright ANSI colors, the 16-color palette, cursor blink, and
scrollback. VTE applies the font, colors, palette, cursor, text-blink mode, and
scrollback. Antialiasing, ANSI interpretation, bold font selection, and dynamic
color escape handling are disabled and labeled as VTE-owned.

### Window

The page exposes title text, optional background image and placement mode,
title components, columns, rows, resize behavior, scrollback limits, restored
rows, and a bookmark field. VTE receives the requested columns and rows.
Profile, shell, directory, process-reported title, and dimensions feed the
window title. TTY and Ctrl-key title components are disabled because VTE does
not report them. The desktop compositor controls pixel geometry and top-level
placement.

### Tab

The page exposes profile, shell, directory, path, job, process, arguments,
dimensions, activity, custom-title, and custom-title component options. Runtime
labels use the selected profile, the shell executable, the current-directory
URI, VTE's reported title, and the terminal's column and row counts. Activity
marks a background tab when its VTE contents change.

### Shell

The page exposes a profile command, run-inside-shell choice, close-on-exit
policies, close confirmation policy, exception names, and shell-exit behavior.
Child exit handling receives the VTE wait status. An automatic clean, error,
or any-exit rule is evaluated first; the clearly labeled fallback can ask,
keep the finished tab open, close the tab, or request a window close. Manual
tab and window closure use one confirmation path and recheck every affected
session before terminating a process. Native builds inspect the PTY foreground
process group and the live executable identity. A login shell is considered
idle only when its device and inode still match the executable that was
launched and no other process remains in its Linux process session. Pending
spawns, background jobs, replaced or same-named executables, and unverified
processes remain protected. After an approved native close, every member of the
tab's isolated Linux process session is re-enumerated while HUP, TERM, and a
final KILL are sent, so known background jobs receive the same escalation as
the foreground command. The work runs off the GTK thread, with 200 milliseconds
after HUP and a full second after TERM for clean shutdown. Every signal uses a
pidfd opened before a full PID, process-start, session, and process-group check,
so the kernel handle cannot retarget a recycled PID. Native Linux deliberately
does not use an unchecked fallback if that identity is unavailable. A sandboxed
Flatpak cannot safely inspect the host process group, so an unknown foreground
process prompts instead of silently closing. Its sandbox-side proxy receives
the same pidfd check before each signal. The app opens a static PIE supervisor,
maps it to fd 3, and asks the broker to execute `/proc/self/fd/3`; a missing
helper therefore fails the spawn instead of running an unsupervised host shell.
The sandbox proxy deliberately leaves VTE's PTY unclaimed. The helper verifies
that Flatpak made it the isolated host session and process-group leader, then
acquires and verifies that PTY as the host session's controlling terminal. The
helper places the payload in its own process group and transfers the terminal
foreground to it before execution. It consumes HUP, INT, QUIT, TERM, USR1, and
USR2 synchronously and sends the staged signals only through pidfds opened
before complete `/proc` revalidation.
Enumeration signals and closes one bound pidfd at a time, so a fork-heavy
session cannot exhaust the helper's descriptor limit. The helper also removes
residual session members when the direct child exits normally. `--watch-bus`
ties the supervisor to the sandbox proxy. Unit tests cover every
automatic/fallback combination, clean, error, and signaled statuses, legacy
migration, disk
round-trips, exception matching, pending spawns, and stale confirmations.
Native acceptance covers non-modal prompts, queued requests, stale-plan
revalidation, JSON reload, the real close-before-spawn cleanup path, and a
three-process shell session whose foreground and background jobs must both be
gone after confirmation. The probe uses per-run process names and verifies the
shell, foreground group, and background group before accepting the prompt.
Flatpak CI supplies unique per-run process markers and uses a host-side pidfd
watcher to prove the same two marked jobs start in the controlling terminal's
foreground and background process groups and both exit after the sandbox
confirms the proxy-backed close.

The Flatpak helper requires Linux 5.3 or newer. It does not fall back to numeric
PID signaling on older kernels. A process that changes into a privilege domain
the user cannot signal may outlive the terminal session; that operating-system
boundary is outside the cleanup guarantee. The helper exits with a failure
status, but this release does not show a dedicated cleanup-error dialog.

### Keyboard

The page exposes Option/Alt-as-Meta, alternate-screen scrolling, and a key
mapping list with add, edit, and remove actions. Chords such as
`Ctrl+Shift+Right` are parsed into a key and exact modifier set. Mapping actions
are validated as bounded encoded PTY sequences and stored separately from
their display labels. New profiles include editable F1 through F12 and shifted
F5 through F12 mappings. Ctrl+C copies when text is selected and sends an
interrupt otherwise. Ctrl+V pastes; Ctrl+Alt+V sends a literal Control-V. VTE
owns alternate-screen scrolling, so that control is disabled.

### Advanced

The page exposes TERM, delete-to-Control-H, non-ASCII escaping, newline paste
conversion, application keypad mode, input scrolling, bells, notifications,
urgency, UTF-8, locale, locale environment setup, and ambiguous-width choice.
Core Terminal wires Delete binding, Control-V escaping, carriage-return paste,
scroll-on-input, audible and visual bells, notifications, urgency, locale, and
CJK ambiguous width to the input or VTE runtime. TERM and locale values are
validated before entering the child environment. VTE owns application keypad
mode and UTF-8 decoding, so those controls are disabled and labeled. Visual
bell output can be limited to profiles where the app's audible bell is off.

### Window Groups

The data model validates group names, profile references, directories, and
dimensions. The settings UI adds, removes, renames, and saves groups. Every
ordered entry can be selected, edited, added, removed, or moved up and down. A
group selected in General launches one tab per entry with its saved profile,
directory, columns, and rows. Launching entries as separate top-level windows
remains deferred. The Linux compositor chooses each top-level window's
position.

### Encodings

The page identifies UTF-8 as the active runtime encoding and shows the 25
legacy names visible in the reference menu as disabled rows. Enable All,
Disable All, and Revert to Defaults are present but disabled. Modern VTE has no
per-session legacy encoding selector.

## Behavior outside Settings

The current release provides search, selection, clipboard actions, tab
navigation, and a terminal menu. It does not claim parity for split panes,
Inspector, marks and bookmarks, print or content export, hyperlink workflows,
dragged-file quoting, remote-connection browsing, or a D-Bus automation API.
These belong in a later feature plan rather than a release claim.

## Platform limits

- The Wayland compositor owns global window placement. Core Terminal can
  request terminal cell dimensions but cannot restore macOS screen coordinates.
- Linux docks do not provide a portable equivalent of live macOS Dock tiles or
  Dock bounce. Notifications and urgency hints are the available alternatives.
- GTK4 and Wayland do not provide a system-wide secure-input mode equivalent to
  macOS Secure Keyboard Entry.
- VTE owns alternate-screen switching, mouse reporting, keypad mode, and
  terminal escape-sequence parsing. Core Terminal does not expose no-op
  overrides for those modes.
- Background blur and inactive-window blur depend on compositor APIs that are
  not portable across Wayland desktops.
- The application can restore text and launch settings, not arbitrary live
  process state after logout.
- Apple fonts, symbols, icons, source, and undocumented private profile data
  are not redistributed.

## KVM acceptance

The GL.iNet Comet message about a hidden pointer is browser Pointer Lock, not a
Core Terminal window. Use Comet Absolute Mode and keep the local cursor
visible. Browser Pointer Lock can still hide or recapture the client-side macOS
pointer when the screenshot tool changes focus. Core Terminal cannot change
that browser state.

The app-side checks are:

- settings, Find, About, and file chooser windows do not make the main window
  modal;
- terminal widgets report `is_mouse_autohide() == false`;
- buttons and stack tabs activate through pointer and keyboard input;
- the profile editor saves a changed color, dimension, locale, and policy;
- a restart reloads the saved values;
- no private profile, screenshot, reference archive, or developer home path is
  present in the package.

## Release evidence

Run the Rust checks, build the package, and inspect it from the repository root:

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --no-fail-fast
cargo audit
scripts/native-acceptance.sh target/release/core-terminal
scripts/build-deb.sh
scripts/check-deb.sh dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
lintian --pedantic dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
scripts/check-private-data.sh dist/core-terminal_0.2.1_$(dpkg --print-architecture).deb
```

Launch the installed package with `GDK_BACKEND=wayland` from the Ubuntu GNOME
desktop. Weston is not an acceptance environment for this project.

## Reference sources

- Apple, [Terminal settings](https://support.apple.com/en-ca/guide/terminal/trml789a1819/mac)
- Apple, [Profile settings](https://support.apple.com/guide/terminal/profiles-change-terminal-windows-trml107/mac)
- Apple, [Import and export profiles](https://support.apple.com/en-asia/guide/terminal/trml4299c696/mac)
- GNOME, [VTE Terminal API](https://gnome.pages.gitlab.gnome.org/vte/gtk4/class.Terminal.html)
- Wayland, [XDG shell protocol](https://wayland.app/protocols/xdg-shell)
- GL.iNet, [Comet Console Guide](https://docs.gl-inet.com/kvm/en/user_guide/gl-rm1/console_guide/)
