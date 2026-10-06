Core Terminal 0.2.3-rc.1 is a local-only release candidate for session-state
retention and cleanup.

- Session save and opt-out clear now share a private advisory lock that remains
  present to avoid inode-replacement races.
- The cleanup path removes only exact interrupted Core Terminal temporary files,
  never unrelated snapshots or application state.
- Session and lock symlinks are rejected; session files must be regular files.
- The native acceptance harness ignores terminated Linux `/proc` task entries
  before deciding whether a tracked child is still live.

This candidate has Cargo/AppStream version `0.2.3-rc.1` and Debian version
`0.2.3~rc1`. The tilde makes an eventual `0.2.3` Debian package supersede this
candidate. It is not a GitHub release, is not tagged, and must not be described
as a published 0.2.2 replacement.

Install only after independent review with:

```sh
sudo apt install ./core-terminal_0.2.3~rc1_amd64.deb
```
