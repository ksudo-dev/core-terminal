Core Terminal 0.2.0 is the first public release.

The release includes:

- an Ubuntu 26.04 `amd64` Debian package, also installation-tested on Debian 13;
- a Flatpak bundle for Linux distributions with Flatpak support;
- a deterministic source archive and CycloneDX dependency SBOM;
- SHA-256 checksums and GitHub build-provenance attestations.

The Flatpak uses the GNOME runtime for GTK and VTE. It does not require the
GNOME desktop. Because Core Terminal is a terminal emulator, the Flatpak has
home-directory access and uses Flatpak's host-command interface to start the
user's host shell.

The settings window includes General, Profiles, Window Groups, and Encodings.
Profiles include Text, Window, Tab, Shell, Keyboard, and Advanced pages. See the
parity matrix for the Linux behaviors that differ from macOS Terminal.

Install the Debian package with:

```sh
sudo apt install ./core-terminal_0.2.0_amd64.deb
```

Install the Flatpak bundle with:

```sh
flatpak install --user ./io.github.ksudo_dev.CoreTerminal.flatpak
flatpak run io.github.ksudo_dev.CoreTerminal
```
