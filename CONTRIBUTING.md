# Contributing

## Before opening a change

Install the packages in the README, then run:

```sh
cargo fmt --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --no-fail-fast
cargo audit
desktop-file-validate packaging/core-terminal.desktop
bash -n scripts/*.sh
scripts/check-doc-style.sh
scripts/security-audit.sh
```

Changes that affect a Debian payload should also run `scripts/build-deb.sh`,
`scripts/check-deb.sh`, and `scripts/check-private-data.sh`.

GTK, VTE, lifecycle, or settings changes also need a native run:

```sh
cargo build --locked --release
scripts/native-acceptance.sh target/release/core-terminal
```

## Scope and review

Describe the user-visible behavior, the affected settings or files, and the
test command that exercises it. Include a screenshot for a GTK layout change.
Keep private profile archives, screenshots, machine-local settings, `target/`,
and `dist/` out of commits. The repository's `.gitignore` covers the generated
directories; review `git status --short` before opening a pull request.

New controls need a runtime path and persistence test. A label or serialized
field without behavior isn't a completed feature.

## Pull requests

Use a focused title, list the checks you ran, and call out Linux or Wayland
limits. Don't claim a check passed when it wasn't run. Maintainers review
security-sensitive changes to shell spawning, profile import, process signals,
URI handling, and package contents before merge.
