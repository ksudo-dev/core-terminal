# Verify a release

Download the release files from the [GitHub Releases page][releases] and keep
them in one directory with `SHA256SUMS`. Run the checksum file before opening
or installing an artifact:

```sh
sha256sum --check SHA256SUMS
```

The release workflow also publishes GitHub build-provenance attestations for
the Debian package, Flatpak bundle, source archive, SBOM, and checksum file.
With a recent GitHub CLI, verify each file against the release and repository:

```sh
release_tag=v0.2.1
repository=ksudo-dev/core-terminal

gh release verify-asset "$release_tag" ./core-terminal_0.2.1_amd64.deb \
  --repo "$repository"
gh release verify-asset "$release_tag" ./io.github.ksudo_dev.CoreTerminal.flatpak \
  --repo "$repository"
gh release verify-asset "$release_tag" ./core-terminal-0.2.1.tar.gz \
  --repo "$repository"
gh release verify-asset "$release_tag" ./core-terminal-0.2.1.cdx.json \
  --repo "$repository"
gh release verify-asset "$release_tag" ./SHA256SUMS \
  --repo "$repository"
```

For direct artifact-attestation verification, use `gh attestation verify` on
an individual file. The signer workflow restriction makes sure the claim was
issued for this repository's release workflow:

```sh
gh attestation verify ./core-terminal_0.2.1_amd64.deb \
  --repo "$repository" \
  --signer-workflow "$repository/.github/workflows/ci.yml"
```

These commands need a GitHub CLI version that provides the `attestation` and
`release verify-asset` commands. If `gh attestation` is unknown, update the
GitHub CLI before treating the verification as complete. A successful command
exits zero and reports the verified attestation; a checksum or provenance
failure means the file must not be installed.

[releases]: https://github.com/ksudo-dev/core-terminal/releases
