# Session retention review checkpoint — 2026-10-05

Base commit: `9875d3504dac85ff5d0c881143da2d1f0ee1ac5d`.

## Included patch scopes

1. **Native acceptance harness liveness** (`src/ui.rs`): ignore terminated
   Linux `/proc` task states before treating a child identity as live. This is
   the already-reviewed 21-line harness patch; it does not alter runtime
   session restoration behavior.
2. **Temporary session/transcript retention** (`src/session_restore.rs`):
   serialize save and opt-out clear with a retained private advisory lock file;
   hold it across cleanup, durable write, rename, and directory sync; clean
   only exact application temporary regular files; reject session symlinks;
   preserve unrelated state files and snapshots.

The lock file is deliberately retained to prevent inode-replacement races.
Only the expected `session.json` path and `.session-<pid>-<nonce>-<sequence>.tmp`
regular-file remnants are handled. No commands, processes, live terminal
transcripts, or user state are included in this checkpoint.

## Regression evidence

- `cargo fmt --check` — passed.
- `cargo check --locked --all-targets` — passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings` — passed.
- `cargo test --locked --all-targets --no-fail-fast` — 147 passed, 0 failed,
  1 ignored (owner-only private fixture).
- `cargo audit --no-fetch --stale` — passed with a copied local advisory DB;
  the global read-only Cargo cache prevented a fresh crates.io index lock.
- Script syntax, documentation style, security audit, release consistency,
  Debian allowlist, and private-data checks — passed.

The new retention package is separate from the previously delivered package:

`artifacts/core-terminal-0.2.2-9875d350-harness-session-retention-20261005/core-terminal_0.2.2_amd64.deb`

SHA-256:

`4f6063a20cf06b7b93e5ea60f4de16f776f60f1b0e6b5d357335153b09a24513`

No package is installed, replaced, published, or committed as a runtime state
artifact. Review this source checkpoint independently on a VM before any
release decision.
