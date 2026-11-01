#!/usr/bin/env bash
# Build target/rsrsbar.app: a release build of rsrsbar in an ad-hoc-signed
# app bundle (native notifications need one).
#
# Usage: scripts/bundle.sh [--install]
#   --install  also replace ~/Applications/rsrsbar.app with the new bundle
#              (the previous one goes to the Trash).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/target/rsrsbar.app"
install_dir="$HOME/Applications"

# Moves $1 to the Trash if anything is there, a dangling symlink included.
discard() {
  if [ -e "$1" ] || [ -L "$1" ]; then
    trash "$1"
  fi
}

install=false
case "${1-}" in
  "") ;;
  --install) install=true ;;
  *) echo "usage: scripts/bundle.sh [--install]" >&2; exit 2 ;;
esac

cd "$root"
mise exec rust@1.99.0 -- cargo build --release --locked
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
