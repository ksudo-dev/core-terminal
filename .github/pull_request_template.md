## Change

Describe the behavior and name the files or settings it changes.

## Checks

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --locked --all-targets --all-features -- -D warnings`
- [ ] `cargo test --locked --all-targets --no-fail-fast`
- [ ] Native Wayland acceptance, when GTK, VTE, lifecycle, or settings changed
- [ ] Debian and security checks, when the package payload changed

## Limits

List any VTE, GTK, GNOME, or Wayland behavior that remains unavailable. Attach
a screenshot when the GTK layout changed.
