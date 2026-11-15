# Local release-candidate verification

Cargo version: `0.2.3-rc.2`.

Debian package version: `0.2.3~rc2`.

This is a local-only candidate. It has no tag, GitHub release, attestation, or
publication workflow. The Debian tilde means a final `0.2.3` package will sort
newer than the candidate.

Validate the generated Debian artifact without installing it:

```sh
scripts/build-deb.sh 0.2.3~rc2
deb=dist/core-terminal_0.2.3~rc2_$(dpkg --print-architecture).deb
scripts/check-deb.sh "$deb"
scripts/check-private-data.sh "$deb"
scripts/security-audit.sh "$deb"
lintian --pedantic "$deb"
sha256sum "$deb"
```

The final-release attestation procedure remains documented separately in
`RELEASE_VERIFICATION.md`; it does not apply to this untagged candidate.
