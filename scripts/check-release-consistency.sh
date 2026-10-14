#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_root"

version=$(sed -n 's/^version = "\([^"]*\)"$/\1/p' Cargo.toml | head -n 1)
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[1-9][0-9]*)?$ ]]; then
  echo "Cargo.toml has an invalid package version: ${version:-missing}" >&2
  exit 1
fi

if [[ "$version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)-rc\.([1-9][0-9]*)$ ]]; then
  debian_version="${BASH_REMATCH[1]}~rc${BASH_REMATCH[2]}"
  release_candidate=1
else
  debian_version="$version"
  release_candidate=0
fi

locked_version=$(
  awk '
    /^name = "core-terminal"$/ { package = 1; next }
    package && /^version = / {
      gsub(/^version = "|"$/, "")
      print
      exit
    }
  ' Cargo.lock
)
if [[ "$locked_version" != "$version" ]]; then
  echo "Cargo.lock version $locked_version does not match Cargo.toml $version" >&2
  exit 1
fi

require_line() {
  local file=$1
  local line=$2
  if ! grep -Fqx -- "$line" "$file"; then
    echo "$file is not aligned with release $version: $line" >&2
    exit 1
  fi
}

require_text() {
  local file=$1
  local value=$2
  if ! grep -Fq -- "$value" "$file"; then
    echo "$file is not aligned with release $version: $value" >&2
    exit 1
  fi
}

require_line .github/workflows/ci.yml "  PACKAGE_VERSION: $version"
require_line .github/workflows/ci.yml "  DEB_VERSION: $debian_version"
require_text scripts/build-deb.sh 'derived_debian_version'
require_line packaging/changelog "core-terminal ($debian_version) unstable; urgency=medium"
require_text packaging/core-terminal.1 "Core Terminal $version"
require_text packaging/io.github.ksudo_dev.CoreTerminal.metainfo.xml \
  "<release version=\"$version\""
require_text README.md "core-terminal_${debian_version}_amd64.deb"
require_text README.md "## Features in $version"
if [[ "$release_candidate" -eq 1 ]]; then
  require_text SECURITY.md "Version $version is the current local release-candidate line;"
else
  require_text SECURITY.md "Version $version is the supported release line."
fi
require_text PARKING_LOT.md "Core Terminal $version is a Linux terminal emulator."
require_text docs/PARITY_MATRIX.md "Terminal $version on Ubuntu GNOME Wayland."
require_text docs/TESTING.md "scripts/build-deb.sh $debian_version"

notes="packaging/release-notes-$version.md"
if [[ ! -s "$notes" ]]; then
  echo "release notes are missing or empty: $notes" >&2
  exit 1
fi

first_changelog_version=$(sed -n 's/^## \([0-9][0-9.rc-]*\)$/\1/p' CHANGELOG.md | head -n 1)
if [[ "$first_changelog_version" != "$version" ]]; then
  echo "CHANGELOG.md starts with release $first_changelog_version, expected $version" >&2
  exit 1
fi

first_appstream_version=$(
  sed -n 's/.*<release version="\([^"]*\)".*/\1/p' \
    packaging/io.github.ksudo_dev.CoreTerminal.metainfo.xml | head -n 1
)
if [[ "$first_appstream_version" != "$version" ]]; then
  echo "AppStream starts with release $first_appstream_version, expected $version" >&2
  exit 1
fi

if [[ "$release_candidate" -eq 1 ]]; then
  require_line .github/workflows/ci.yml '    if: ${{ false }}'
  require_text docs/RELEASE_CANDIDATE_VERIFICATION.md "Cargo version: \`$version\`."
  require_text docs/RELEASE_CANDIDATE_VERIFICATION.md "Debian package version: \`$debian_version\`."
else
  require_text .github/workflows/ci.yml "refs/tags/v$version"
  require_text .github/workflows/ci.yml "packaging/release-notes-$version.md"
  require_line docs/RELEASE_VERIFICATION.md "release_tag=v$version"
fi

printf 'release metadata is consistent for Core Terminal %s\n' "$version"
