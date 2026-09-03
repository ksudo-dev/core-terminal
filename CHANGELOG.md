# Changelog

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
- Documented VTE and Wayland behavior that cannot match macOS Terminal.

The release binary passed 52 automated tests plus the opt-in private fixture
test. Its native Wayland acceptance report recorded all settings pages present,
profile persistence, applied VTE properties, non-modal settings, pointer
autohide disabled, readable profile names, and unclipped profile navigation.
