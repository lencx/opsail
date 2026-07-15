#!/bin/sh

set -eu

fail() {
  printf 'opsail installer: %s\n' "$1" >&2
  exit 1
}

for command_name in awk chmod curl install mkdir mktemp rm tar uname; do
  command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done

: "${HOME:?opsail installer: HOME is not set}"

version="${OPSAIL_VERSION:-latest}"
install_dir="${OPSAIL_INSTALL_DIR:-$HOME/.local/bin}"

case "$install_dir" in
  /*) ;;
  *) fail "install directory must be an absolute path: $install_dir" ;;
esac

case "$install_dir" in
  *:*) fail "install directory cannot contain a colon: $install_dir" ;;
esac

system="$(uname -s)"
machine="$(uname -m)"

case "$system:$machine" in
  Darwin:arm64 | Darwin:aarch64)
    target="aarch64-apple-darwin"
    ;;
  Darwin:x86_64 | Darwin:amd64)
    target="x86_64-apple-darwin"
    ;;
  Linux:x86_64 | Linux:amd64)
    target="x86_64-unknown-linux-musl"
    ;;
  Linux:aarch64 | Linux:arm64)
    target="aarch64-unknown-linux-musl"
    ;;
  *)
    fail "unsupported platform: $system $machine"
    ;;
esac

if [ "$version" = "latest" ]; then
  release_url="https://github.com/lencx/opsail/releases/latest/download"
else
  case "$version" in
    '' | *[!0-9A-Za-z._-]*)
      fail "invalid version: $version"
      ;;
  esac

  case "$version" in
    v*) tag="$version" ;;
    *) tag="v$version" ;;
  esac
  release_url="https://github.com/lencx/opsail/releases/download/$tag"
fi

asset="opsail-$target.tar.gz"
temp_root="${TMPDIR:-/tmp}"

case "$temp_root" in
  /*) ;;
  *) fail "temporary directory must be an absolute path: $temp_root" ;;
esac

temp_dir="$(mktemp -d "$temp_root/opsail.XXXXXX")"

cleanup() {
  rm -rf "$temp_dir"
}

on_signal() {
  trap - 0 HUP INT TERM
  cleanup
  exit 1
}

trap cleanup 0
trap on_signal HUP INT TERM

download() {
  source_url="$1"
  destination="$2"

  curl \
    --proto '=https' \
    --proto-redir '=https' \
    --tlsv1.2 \
    --fail \
    --silent \
    --show-error \
    --location \
    --url "$source_url" \
    --output "$destination"
}

archive_path="$temp_dir/$asset"
checksums_path="$temp_dir/SHA256SUMS"

printf 'Downloading opsail for %s...\n' "$target"
download "$release_url/$asset" "$archive_path"
download "$release_url/SHA256SUMS" "$checksums_path"

expected_hash="$(awk -v asset="$asset" '$2 == asset { print $1; exit }' "$checksums_path")"
[ -n "$expected_hash" ] || fail "checksum not found for $asset"

case "$expected_hash" in
  *[!0-9A-Fa-f]*) fail "invalid checksum for $asset" ;;
esac

[ "${#expected_hash}" -eq 64 ] || fail "invalid checksum for $asset"

if command -v sha256sum >/dev/null 2>&1; then
  actual_hash="$(sha256sum "$archive_path" | awk '{ print $1 }')"
elif command -v shasum >/dev/null 2>&1; then
  actual_hash="$(shasum -a 256 "$archive_path" | awk '{ print $1 }')"
else
  fail "sha256sum or shasum is required to verify the download"
fi

[ "$actual_hash" = "$expected_hash" ] || fail "checksum verification failed for $asset"

tar -xzf "$archive_path" -C "$temp_dir"
binary_path="$temp_dir/opsail-$target/opsail"
[ -f "$binary_path" ] || fail "downloaded archive does not contain opsail"

chmod 755 "$binary_path"
if ! installed_version="$("$binary_path" --version)"; then
  fail "downloaded opsail binary could not be executed"
fi

mkdir -p "$install_dir"
install -m 755 "$binary_path" "$install_dir/opsail"

printf '%s\n' "$installed_version"
printf 'Installed opsail to %s/opsail\n' "$install_dir"

case ":${PATH:-}:" in
  *":$install_dir:"*)
    ;;
  *)
    printf 'The install directory is not on PATH: %s\n' "$install_dir"
    printf 'Add that directory to your shell profile, then open a new terminal.\n'
    ;;
esac
