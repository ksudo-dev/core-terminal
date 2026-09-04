# Verify a release

Use a current GitHub CLI that provides `gh attestation verify`. Download all
files into a new directory, then verify checksums before opening or installing
anything:

```sh
set -eu
release_tag=v0.2.1
repository=ksudo-dev/core-terminal
release_dir=$(mktemp -d)

gh attestation verify --help >/dev/null
gh release download "$release_tag" --repo "$repository" --dir "$release_dir"
(
  cd "$release_dir"
  sha256sum --check SHA256SUMS
)
```

The release workflow also publishes GitHub build-provenance attestations for
the Debian package, Flatpak bundle, source archive, SBOM, and checksum file.
With a recent GitHub CLI, verify each file against the repository, release tag,
and workflow that built it:

```sh
source_digest=$(gh api "repos/$repository/commits/$release_tag" --jq .sha)
case "$source_digest" in
  ''|*[!0-9A-Fa-f]*) echo "invalid source commit" >&2; exit 1 ;;
esac
[ "${#source_digest}" -eq 40 ] || {
  echo "invalid source commit length" >&2
  exit 1
}

for asset in \
  core-terminal_0.2.1_amd64.deb \
  io.github.ksudo_dev.CoreTerminal.flatpak \
  core-terminal-0.2.1.tar.gz \
  core-terminal-0.2.1.cdx.json \
  SHA256SUMS
do
  gh attestation verify "$release_dir/$asset" \
    --repo "$repository" \
    --signer-workflow "$repository/.github/workflows/ci.yml" \
    --source-ref "refs/tags/$release_tag" \
    --source-digest "$source_digest" \
    --deny-self-hosted-runners
done
```

A successful command exits zero and reports a verified attestation. These
checks establish origin and integrity; they do not prove that an artifact is
vulnerability-free. If any check fails, do not install the files.

[releases]: https://github.com/ksudo-dev/core-terminal/releases
