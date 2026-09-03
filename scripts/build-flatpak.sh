#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
app_id=io.github.ksudo_dev.CoreTerminal
manifest="$repo_root/packaging/${app_id}.yml"
build_dir="$repo_root/target/flatpak/build"
repository_dir="$repo_root/target/flatpak/repository"
bundle="$repo_root/dist/${app_id}.flatpak"

if [[ -z "${SOURCE_DATE_EPOCH:-}" ]]; then
  SOURCE_DATE_EPOCH=$(git -C "$repo_root" show -s --format=%ct HEAD 2>/dev/null || printf '0')
fi
if [[ ! "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]]; then
  echo "SOURCE_DATE_EPOCH must be an integer" >&2
  exit 1
fi
export SOURCE_DATE_EPOCH

for command in flatpak flatpak-builder; do
  if ! command -v "$command" >/dev/null; then
    echo "missing command: $command" >&2
    exit 1
  fi
done

if ! flatpak remotes --user --columns=name | grep -Fxq flathub; then
  echo "the user Flatpak installation needs a remote named flathub" >&2
  exit 1
fi

rm -rf -- "$build_dir" "$repository_dir"
install -d "$repo_root/dist" "$(dirname "$build_dir")"
flatpak-builder \
  --user \
  --install-deps-from=flathub \
  --force-clean \
  --repo="$repository_dir" \
  --default-branch=stable \
  "$build_dir" \
  "$manifest"
flatpak build-bundle \
  "$repository_dir" \
  "$bundle" \
  "$app_id" \
  stable \
  --runtime-repo=https://flathub.org/repo/flathub.flatpakrepo

test -s "$bundle"
printf '%s\n' "$bundle"
