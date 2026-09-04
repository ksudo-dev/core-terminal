# Changelog

## Unreleased

- Separated startup, new-window, new-tab, and window-group launch policies so
  one path cannot silently override another.
- Made same-profile tabs follow the active tab and preserve the chosen working
  directory in the session model.
- Kept startup-profile changes independent from live tabs when Settings is
  saved, and reapplied edited runtime profile properties to every matching tab.
- Preserved the saved default profile while switching tabs, and kept unrelated
  startup choices stable when custom profiles are removed.
- Added the missing startup-profile choice to the new-window policy control.

## 0.2.1

Released 2026-09-03.

- Added a complete ordered-entry editor for Window Groups, with visible tab
  summaries and labeled add, remove, move, and launch actions.
- Made the bottom Settings Save action commit Window Group drafts. Switching
  groups and launching a group also retain current edits.
- Made group renames atomic, refreshed profile selectors after profile
  mutations, and blocked deletion of profiles still used by a group.
- Added bounded absolute-path validation for saved group directories.
- Corrected Flatpak host-shell startup so it uses the host login shell, PATH,
  D-Bus session, agent sockets, and desktop environment.
- Removed VTE development files from the Flatpak and expanded installed-bundle,
  host-bridge, security, and release-verification checks.
- Reworked public documentation and metadata around the supported Debian and
  x86_64 Flatpak artifacts.

## 0.2.0

Released 2026-09-03.

- Added the four settings panes: General, Profiles, Window Groups, and
  Encodings.
- Added six profile pages: Text, Window, Tab, Shell, Keyboard, and Advanced.
- Added profile import and export paths for supported `.terminal` plist fields.
- Added profile mutation and window-group persistence paths.
- Added startup window-group launch with ordered profile, directory, and size
  entries.
- Added selection-aware clipboard shortcuts, editable PTY key mappings,
  Option/Alt Meta input, Control-H Delete, non-ASCII escaping, and
  carriage-return paste.
- Added the standard function-key mapping table, custom-title component policy,
  visual-bell condition, and the full reference encoding list.
- Wired text blinking, CJK ambiguous width, visual bell, background
  notifications, urgency, tab activity, and title components to GTK or VTE.
- Disabled renderer-owned and unavailable controls with an explanation instead
  of storing values that have no runtime effect.
- Disabled VTE pointer autohide and kept every secondary window non-modal for
  KVM and screenshot focus recovery.
- Rebuilt the Profiles layout so horizontal wheel input cannot hide the
  sidebar, profile names, or editor tabs. Profile actions now use visible text
  labels in a bounded grid.
- Added native allocation checks for the settings window, profile sidebar,
  profile names, profile tabs, and action buttons.
- Added explicit Debian packaging checks for desktop metadata, installed files,
  license notices, and private reference data.
- Added an offline, checksummed Flatpak build using the GNOME 50 runtime and a
  deliberate host-shell bridge for use on non-Debian distributions.
- Adopted `io.github.ksudo_dev.CoreTerminal` as the application and Flatpak ID.
- Documented VTE and Wayland behavior that cannot match macOS Terminal.

The release binary passed 56 automated tests plus the opt-in private fixture
test. Its native Wayland acceptance report recorded all settings pages present,
profile persistence, applied VTE properties, non-modal settings, pointer
autohide disabled, readable profile names, and unclipped profile navigation.
