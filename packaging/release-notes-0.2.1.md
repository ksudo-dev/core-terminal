Core Terminal 0.2.1 tightens Window Group editing and Flatpak shell startup.

- Every ordered tab in a Window Group can be selected, edited, added, removed,
  or moved.
- The bottom Settings Save button now persists the current group draft. Group
  switching and launching retain current edits, and renames are atomic.
- Profiles used by a saved group cannot be deleted until the reference is
  removed. Saved group directories must be bounded absolute paths.
- The Flatpak now starts the host login shell with the host desktop session
  environment, while preserving custom-command arguments and working
  directories.
- Flatpak package checks now verify required VTE runtime files and reject
  bundled development files.

The Debian package targets Ubuntu 26.04 `amd64` and is installation-tested on
Debian 13. The Flatpak bundle targets x86_64 Linux systems with Flatpak support
and does not require the GNOME desktop.

Install the Debian package with:

```sh
sudo apt install ./core-terminal_0.2.1_amd64.deb
```

Install the Flatpak bundle with:

```sh
flatpak install --user ./io.github.ksudo_dev.CoreTerminal.flatpak
flatpak run io.github.ksudo_dev.CoreTerminal
```
