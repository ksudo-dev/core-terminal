#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
arch=$(dpkg --print-architecture)
app_id=io.github.ksudo_dev.CoreTerminal
build_root="$repo_root/target/debian/core-terminal"
deps_root="$repo_root/target/debian/shlibdeps"

if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH=$(git -C "$repo_root" show -s --format=%ct HEAD 2>/dev/null || printf '0')
fi
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be a non-negative integer" >&2
  exit 1
fi
export SOURCE_DATE_EPOCH

if [[ ! -f "$repo_root/Cargo.toml" ]]; then
  echo "Cargo.toml is missing; run this from a complete Core Terminal checkout" >&2
  exit 1
fi

manifest_package_id=$(cargo pkgid --manifest-path "$repo_root/Cargo.toml")
if [[ "$manifest_package_id" == *"@"* ]]; then
  manifest_version=${manifest_package_id##*@}
else
  manifest_version=${manifest_package_id##*#}
fi

# Cargo uses SemVer prereleases (0.2.3-rc.1); Debian needs a tilde
# prerelease (0.2.3~rc1) so the eventual 0.2.3 package supersedes this RC.
if [[ "$manifest_version" =~ ^([0-9]+\.[0-9]+\.[0-9]+)-rc\.([1-9][0-9]*)$ ]]; then
  derived_debian_version="${BASH_REMATCH[1]}~rc${BASH_REMATCH[2]}"
elif [[ "$manifest_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  derived_debian_version="$manifest_version"
else
  echo "unsupported Cargo package version: $manifest_version" >&2
  exit 1
fi
version=${1:-$derived_debian_version}
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+~.-][0-9A-Za-z.+~-]+)?$ ]]; then
  echo "invalid Debian version: $version" >&2
  exit 1
fi
if [[ "$version" != "$derived_debian_version" ]]; then
  echo "Debian package version $version does not match Cargo.toml $manifest_version (expected $derived_debian_version)" >&2
  exit 1
fi
deb_path="$repo_root/dist/core-terminal_${version}_${arch}.deb"

for required in \
  "$repo_root/packaging/core-terminal.desktop" \
  "$repo_root/packaging/io.github.ksudo_dev.CoreTerminal.metainfo.xml" \
  "$repo_root/packaging/core-terminal.1" \
  "$repo_root/packaging/changelog" \
  "$repo_root/packaging/lintian-overrides" \
  "$repo_root/packaging/copyright" \
  "$repo_root/THIRD_PARTY_NOTICES.md" \
  "$repo_root/data/default-profiles.json"; do
  if [[ ! -f "$required" ]]; then
    echo "required packaging input is missing: $required" >&2
    exit 1
  fi
done

cargo_home_path=${CARGO_HOME:-${HOME}/.cargo}
remap_flags="--remap-path-prefix=$repo_root=/usr/src/core-terminal"
if [[ -d "$cargo_home_path" ]]; then
  remap_flags+=" --remap-path-prefix=$cargo_home_path=/usr/src/cargo"
fi
RUSTFLAGS="${RUSTFLAGS:-} $remap_flags" \
  cargo build --locked --release --manifest-path "$repo_root/Cargo.toml"
binary="$repo_root/target/release/core-terminal"
if [[ ! -x "$binary" ]]; then
  echo "release binary not found: $binary" >&2
  exit 1
fi
if strings "$binary" | grep -Eq '/home/[^/]+/|/Users/[^/]+/'; then
  echo "release binary contains an absolute developer home path" >&2
  exit 1
fi

rm -rf "$build_root" "$deps_root"
install -d "$build_root/DEBIAN" \
  "$build_root/usr/bin" \
  "$build_root/usr/share/applications" \
  "$build_root/usr/share/metainfo" \
  "$build_root/usr/share/core-terminal" \
  "$build_root/usr/share/doc/core-terminal" \
  "$build_root/usr/share/man/man1" \
  "$build_root/usr/share/lintian/overrides" \
  "$build_root/usr/share/icons/hicolor"
install -m 0755 "$binary" "$build_root/usr/bin/core-terminal"
install -m 0644 "$repo_root/packaging/core-terminal.desktop" \
  "$build_root/usr/share/applications/${app_id}.desktop"
install -m 0644 "$repo_root/packaging/${app_id}.metainfo.xml" \
  "$build_root/usr/share/metainfo/${app_id}.metainfo.xml"
install -m 0644 "$repo_root/data/default-profiles.json" \
  "$build_root/usr/share/core-terminal/default-profiles.json"
install -m 0644 "$repo_root/LICENSE" "$build_root/usr/share/doc/core-terminal/LICENSE"
install -m 0644 "$repo_root/packaging/copyright" \
  "$build_root/usr/share/doc/core-terminal/copyright"
install -m 0644 "$repo_root/THIRD_PARTY_NOTICES.md" \
  "$build_root/usr/share/doc/core-terminal/THIRD_PARTY_NOTICES.md"
install -m 0644 "$repo_root/packaging/changelog" \
  "$build_root/usr/share/doc/core-terminal/changelog"
gzip -n -9 "$build_root/usr/share/doc/core-terminal/changelog"
install -m 0644 "$repo_root/packaging/lintian-overrides" \
  "$build_root/usr/share/lintian/overrides/core-terminal"
install -m 0644 "$repo_root/packaging/core-terminal.1" \
  "$build_root/usr/share/man/man1/core-terminal.1"
gzip -n -9 "$build_root/usr/share/man/man1/core-terminal.1"

install -d "$deps_root/debian/core-terminal/DEBIAN" \
  "$deps_root/debian/core-terminal/usr/bin"
install -m 0644 "$repo_root/packaging/shlibdeps.control" "$deps_root/debian/control"
install -m 0755 "$binary" "$deps_root/debian/core-terminal/usr/bin/core-terminal"
depends=$(
  cd "$deps_root"
  dpkg-shlibdeps -O debian/core-terminal/usr/bin/core-terminal \
    | sed -n 's/^shlibs:Depends=//p'
)
if [[ -z "$depends" ]]; then
  echo "dpkg-shlibdeps did not produce shared-library dependencies" >&2
  exit 1
fi

for size in 32 64 128 256 512; do
  install -d "$build_root/usr/share/icons/hicolor/${size}x${size}/apps"
  install -m 0644 "$repo_root/data/icons/core-terminal-icon-${size}.png" \
    "$build_root/usr/share/icons/hicolor/${size}x${size}/apps/${app_id}.png"
done

sed -e "s/__VERSION__/$version/g" -e "s/__ARCH__/$arch/g" \
  -e "s/__DEPENDS__/$depends/g" \
  "$repo_root/packaging/control.template" > "$build_root/DEBIAN/control"
chmod 0644 "$build_root/DEBIAN/control"

find "$build_root" -type d -exec chmod 0755 {} +
(
  cd "$build_root"
  find usr -type f -print0 | sort -z | xargs -0 md5sum > DEBIAN/md5sums
)
chmod 0644 "$build_root/DEBIAN/md5sums"

install -d "$repo_root/dist"
dpkg-deb --root-owner-group --build "$build_root" "$deb_path"
"$repo_root/scripts/check-private-data.sh" "$deb_path"
printf '%s\n' "$deb_path"
