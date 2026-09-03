# Deferred compatibility and scope

Core Terminal 0.2.0 is a Linux terminal emulator. The following items are
outside its current parity claim:

- exact macOS Dock tile contents and Dock bounce
- exact global window coordinates under GNOME Wayland
- live process or session restoration after logout
- a secure keyboard mode equivalent to macOS Secure Keyboard Entry
- legacy per-session encodings through the current VTE API
- split panes that share one terminal screen and scrollback
- a Terminal.app Inspector equivalent
- marks, bookmarks, print, and terminal-content export
- RPM packaging
- AppleScript compatibility

Window Groups persist ordered profile, directory, and terminal-size entries.
A startup group launches those entries as tabs. The current editor changes the
first entry while preserving extra entries loaded from disk. A later editor can
add entry-by-entry controls, separate top-level windows, and saved window state.
GNOME chooses placement.

The Encodings page identifies UTF-8 as active and labels legacy encodings as
unavailable. It does not pretend that disabled entries work.

The supplied screenshots, private profile archive, and reference archive are
reference material only. They are not read by the application, committed to
Git, or copied into a package. The supported runtime profile file is
`data/default-profiles.json`.
