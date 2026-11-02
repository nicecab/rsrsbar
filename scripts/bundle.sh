#!/usr/bin/env bash
# Build target/rsrsbar.app: a release build of rsrsbar in an ad-hoc-signed
# app bundle (native notifications need one).
#
# Usage: scripts/bundle.sh [--install]
#        scripts/bundle.sh --zip <dir>
#   --install  also replace ~/Applications/rsrsbar.app with the new bundle
#              (the previous one goes to the Trash).
#   --zip      also write the release zip and its checksum into <dir>:
#              rsrsbar-v<version>-aarch64-apple-darwin.zip and .zip.sha256.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/rsrsbar.app"
install_dir="$HOME/Applications"

usage() {
  echo "usage: scripts/bundle.sh [--install | --zip <dir>]" >&2
  exit 2
}

# Moves $1 to the Trash if anything is there, a dangling symlink included.
discard() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    trash "$1"
  fi
}

install=false
zip_dir=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --install) install=true ;;
    --zip)
      # The directory must follow, and must not be the next option: taking
      # `--install` as a directory name would hide the flag conflict.
      case "${2-}" in
        "" | -*) usage ;;
      esac
      zip_dir=$2
      shift
      ;;
    *) usage ;;
  esac
  shift
done
# One output per run: a release zip is never also installed locally.
if [ "$install" = true ] && [ -n "$zip_dir" ]; then
  usage
fi

# CI installs the toolchain pinned by rust-toolchain.toml with rustup.
# Locally the toolchain comes from mise; its rustup shim is on PATH but
# fails without a configured version, so rustup is probed by running it.
if rustup --version >/dev/null 2>&1; then
  cargo=(cargo)
else
  cargo=(mise exec rust@1.99.0 -- cargo)
fi

cd "$root"
"${cargo[@]}" build --release --locked
exe="$root/target/release/rsrsbar"

# A stale bundle (or a dangling symlink in its place) is replaced, never
# merged into.
discard "$app"
mkdir -p "$app/Contents/MacOS"
# The Info.plist comes from the very binary being bundled. It is run from
# target/release: run from inside the half-built bundle, its write to
# Contents/Info.plist failed with EIO on macOS 27.
"$exe" info-plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null
cp "$exe" "$app/Contents/MacOS/rsrsbar"

# Sign last: any change to the bundle afterwards breaks the seal. The
# signing identifier is the bundle identifier the binary itself declares.
identifier=$(plutil -extract CFBundleIdentifier raw "$app/Contents/Info.plist")
codesign --force --sign - --identifier "$identifier" "$app"
codesign --verify --strict "$app"
echo "built $app"

if [ "$install" = true ]; then
  mkdir -p "$install_dir"
  discard "$install_dir/rsrsbar.app"
  # ditto keeps the signature's extended attributes intact.
  ditto "$app" "$install_dir/rsrsbar.app"
  codesign --verify --strict "$install_dir/rsrsbar.app"
  echo "installed $install_dir/rsrsbar.app"
fi

if [ -n "$zip_dir" ]; then
  # The name carries the version the bundle declares, so the zip can never
  # disagree with what it contains.
  version=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")
  name="rsrsbar-v$version-aarch64-apple-darwin.zip"
  mkdir -p "$zip_dir"
  zip_dir=$(cd "$zip_dir" && pwd)
  zip="$zip_dir/$name"
  discard "$zip"
  discard "$zip.sha256"
  # ditto, unlike zip(1), keeps the signature's extended attributes, and
  # --keepParent puts rsrsbar.app itself at the archive's root.
  ditto -c -k --keepParent "$app" "$zip"
  # Run from the zip's directory so the checksum file names the bare file
  # and `shasum -a 256 -c` works wherever the two files are downloaded.
  (cd "$zip_dir" && shasum -a 256 "$name" > "$name.sha256")
  echo "$zip"
  echo "$zip.sha256"
fi
