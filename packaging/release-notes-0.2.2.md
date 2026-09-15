Core Terminal 0.2.2 fixes profile editing and strengthens process cleanup.

- The Text profile page again shows its font, cursor, and scrollback controls.
  Settings load from the selected profile and keep imported values that Linux
  cannot edit.
- Scrollback is canonical per profile on the Text page, with an explicit
  unlimited state and support for project values such as Man Page's 48 lines.
  The Window-page field is a read-only compatibility mirror.
- Upgrades copy legacy global input-scroll, bell, and bright-bold behavior into
  every profile once, keeping the saved profile values aligned with runtime
  behavior.
- Homebrew now stores `Monospace` and `12` as separate font family and size
  values.
- Startup, new-window, new-tab, and Window Group policies remain independent.
  Editing one no longer silently changes another.
- Tab and window closure revalidates the affected process session before each
  signal. Native and Flatpak paths clean foreground and background jobs without
  trusting a recycled numeric PID.
- PTY tests now exercise resize delivery, foreground job suspension, `fg`,
  Ctrl-C, and shell exit-status preservation.
- Core Terminal now uses a standard menubar, text-labeled tab controls, and a
  terminal right-click menu. The titlebar stays limited to the app title and
  compositor window controls.
- The profile editor no longer enforces a desktop-sized minimum or fixed inner
  widths, so narrow settings windows keep profile names and page content
  reachable.

The Debian package targets Ubuntu 26.04 `amd64` and is installation-tested on
Debian 13. The Flatpak bundle targets x86_64 Linux systems with Flatpak support
and does not require the GNOME desktop.

Install the Debian package with:

```sh
sudo apt install ./core-terminal_0.2.2_amd64.deb
```

Install the Flatpak bundle with:

```sh
flatpak install --user ./io.github.ksudo_dev.CoreTerminal.flatpak && flatpak run --user io.github.ksudo_dev.CoreTerminal
```
