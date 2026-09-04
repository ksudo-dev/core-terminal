Core Terminal 0.2.2 fixes profile editing and strengthens process cleanup.

- The Text profile page again shows its font, cursor, and scrollback controls.
  Settings load from the selected profile and keep imported values that Linux
  cannot edit.
- Startup, new-window, new-tab, and Window Group policies remain independent.
  Editing one no longer silently changes another.
- Tab and window closure revalidates the affected process session before each
  signal. Native and Flatpak paths clean foreground and background jobs without
  trusting a recycled numeric PID.
- PTY tests now exercise resize delivery, foreground job suspension, `fg`,
  Ctrl-C, and shell exit-status preservation.

The Debian package targets Ubuntu 26.04 `amd64` and is installation-tested on
Debian 13. The Flatpak bundle targets x86_64 Linux systems with Flatpak support
and does not require the GNOME desktop.

Install the Debian package with:

```sh
sudo apt install ./core-terminal_0.2.2_amd64.deb
```

Install the Flatpak bundle with:

```sh
flatpak install --user ./io.github.ksudo_dev.CoreTerminal.flatpak
flatpak run io.github.ksudo_dev.CoreTerminal
```
