#!/usr/bin/env bash
# Check a release zip written by `scripts/bundle.sh --zip` before it ships:
# its checksum, its signature after unpacking, its Info.plist and the
# version the binary reports.
#
# Usage: scripts/verify-package.sh <zip> <expected-version>
#   The version under test is the argument, never read from Cargo.toml, so
#   a downloaded release can be checked against the tag it was published as.
#   Exits non-zero naming the first check that failed.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: scripts/verify-package.sh <zip> <expected-version>" >&2
  exit 2
fi
zip=$1
expected=$2

fail() {
  echo "verify-package: FAIL: $*" >&2
  exit 1
}

# 1. Checksum, from the zip's directory: the .sha256 names the bare file.
# `shasum -c` verifies whichever files the record names, so the record must
# first be shown to be exactly one line naming this zip; otherwise a
# checksum file for some other file would pass for this one.
name=$(basename "$zip")
test -f "$zip" || fail "no zip at $zip"
test -f "$zip.sha256" || fail "no checksum file at $zip.sha256"
record=$(cat "$zip.sha256")
lines=$(wc -l < "$zip.sha256" | tr -d ' ')
pattern='^[0-9a-f]{64}  ([^[:cntrl:]]+)$'
if [ "$lines" != 1 ] || ! [[ "$record" =~ $pattern ]] || [ "${BASH_REMATCH[1]}" != "$name" ]; then
  fail "$name.sha256 is not exactly one '<sha256>  $name' line"
fi
(cd "$(dirname "$zip")" && shasum -a 256 -c "$name.sha256" >/dev/null) ||
  fail "checksum mismatch for $name"

# 2. Unpack the way a downloader would, into a directory removed on exit.
# On GitHub's runners it lives in RUNNER_TEMP, which the runner empties after
# every job, so CI does not depend on a `trash` command; elsewhere it is
# moved to the Trash, and a failure to do so is reported, not hidden.
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/verify-package.XXXXXX")
cleanup() {
  if [ -z "${RUNNER_TEMP:-}" ] && ! trash "$work" 2>/dev/null; then
    echo "verify-package: warning: could not move $work to the Trash" >&2
  fi
}
trap cleanup EXIT
ditto -x -k "$zip" "$work" || fail "cannot unpack $zip"
app="$work/rsrsbar.app"
test -d "$app" || fail "zip has no rsrsbar.app at its root"

# 3. The ad-hoc signature must survive zipping and unpacking.
codesign --verify --strict "$app" || fail "codesign --verify --strict"

# 4. Info.plist identity, version and menu-bar-only flag.
plist="$app/Contents/Info.plist"
# `raw` prints a boolean and a string alike, so each key's type is enforced
# with -expect; a value of the wrong type reads as empty.
key() {
  plutil -extract "$1" raw -expect "$2" "$plist" 2>/dev/null || true
}
identifier=$(key CFBundleIdentifier string)
[ "$identifier" = com.nicecab.rsrsbar ] ||
  fail "CFBundleIdentifier is '$identifier', expected com.nicecab.rsrsbar"
short_version=$(key CFBundleShortVersionString string)
[ "$short_version" = "$expected" ] ||
  fail "CFBundleShortVersionString is '$short_version', expected $expected"
ui_element=$(key LSUIElement bool)
if [ "$ui_element" != true ]; then
  fail "LSUIElement is not the boolean true (type: $(plutil -type LSUIElement "$plist" 2>/dev/null || echo missing))"
fi

# 5. The binary itself reports the same version.
reported=$("$app/Contents/MacOS/rsrsbar" --version) || fail "rsrsbar --version failed"
[ "$reported" = "rsrsbar $expected" ] ||
  fail "rsrsbar --version printed '$reported', expected 'rsrsbar $expected'"

echo "verify-package: OK $name (rsrsbar $expected)"
