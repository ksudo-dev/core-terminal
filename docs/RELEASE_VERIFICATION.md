# Verify a release

Download the release files from the [GitHub Releases page][releases] and keep
them in one directory with `SHA256SUMS`. Run the checksum file before opening
or installing an artifact:

```sh
sha256sum --check SHA256SUMS
```

The release workflow also publishes GitHub build-provenance attestations for
the Debian package, Flatpak bundle, source archive, SBOM, and checksum file.
With a recent GitHub CLI, verify each file against the repository, release tag,
and workflow that built it:

```sh
release_tag=v0.2.1
repository=ksudo-dev/core-terminal
source_digest=$(gh api "repos/$repository/commits/$release_tag" --jq .sha)

for artifact in \
  core-terminal_0.2.1_amd64.deb \
  io.github.ksudo_dev.CoreTerminal.flatpak \
  core-terminal-0.2.1.tar.gz \
  core-terminal-0.2.1.cdx.json \
  SHA256SUMS
do
  gh attestation verify "./$artifact" \
    --repo "$repository" \
    --signer-workflow "$repository/.github/workflows/ci.yml" \
    --source-ref "refs/tags/$release_tag" \
    --source-digest "$source_digest"
done
```

These commands need a GitHub CLI version that provides `gh attestation`. If
that command is unknown, update GitHub CLI before treating verification as
complete. A successful command exits zero and reports the verified
attestation; a checksum or provenance failure means the file must not be
installed.

[releases]: https://github.com/ksudo-dev/core-terminal/releases
