Core Terminal 0.2.3-rc.2 is a local-only release candidate.

- Native file drops paste quoted local paths without executing them.
- Saved SSH/SFTP connections store connection metadata only and launch a new
  terminal with a validated direct argument vector.
- Split scrollback shows a bounded, refreshable, read-only snapshot beside the
  live prompt; it never creates a second shell or persists captured output.
- Saved multi-window layouts now suppress configured profile command replay.

This candidate has Cargo/AppStream version `0.2.3-rc.2` and Debian version
`0.2.3~rc2`. The tilde makes an eventual `0.2.3` Debian package supersede this
candidate. It is not a GitHub release, is not tagged, and must not be described
as a published 0.2.2 replacement.

Install only after independent review with:

```sh
sudo apt install ./core-terminal_0.2.3~rc2_amd64.deb
```
