#!/usr/bin/env bash
# Packages git-drive Files for macOS as a disk image: builds the .app, puts it
# beside a link to /Applications in a compressed .dmg (hdiutil, no Finder
# scripting), and signs both. With Apple credentials it signs with the
# Developer ID, notarizes and staples, so the app opens on any Mac without a
# warning; without them it signs ad hoc, and the first open on another Mac
# needs System Settings, Privacy & Security, Open Anyway.
#
#   APPLE_SIGNING_IDENTITY  "Developer ID Application: Name (TEAMID)"; absent = ad hoc ("-")
#   APPLE_ID, APPLE_PASSWORD (an app-specific password), APPLE_TEAM_ID
#                           notarization, with a Developer ID only
#   UNIVERSAL=1             Apple silicon and Intel in one app
#                           (needs: rustup target add x86_64-apple-darwin)
#
# Prints the disk image's path last.
set -euo pipefail

here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/.." && pwd)
identity=${APPLE_SIGNING_IDENTITY:--}
version=$(node -p "require('$here/package.json').version")
notarize=0
if [ "$identity" != "-" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  notarize=1
fi

build_args=(--bundles app)
release="$root/target/release"
arch=$(uname -m)
if [ "${UNIVERSAL:-0}" = 1 ]; then
  build_args+=(--target universal-apple-darwin)
  release="$root/target/universal-apple-darwin/release"
  arch=universal
fi

echo "==> building the app (identity: $identity)"
# Tauri signs the whole bundle with this identity, and with the Apple
# variables set it also notarizes and staples the app itself.
if [ "$notarize" = 1 ]; then
  (cd "$here" && APPLE_SIGNING_IDENTITY="$identity" npm run tauri build -- "${build_args[@]}")
else
  (cd "$here" && env -u APPLE_ID -u APPLE_PASSWORD -u APPLE_TEAM_ID APPLE_SIGNING_IDENTITY="$identity" npm run tauri build -- "${build_args[@]}")
fi
app="$release/bundle/macos/git-drive Files.app"
codesign --verify --deep --strict "$app"

echo "==> making the disk image"
out="$release/bundle/dmg"
mkdir -p "$out"
dmg="$out/git-drive-Files-$version-$arch.dmg"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
ditto "$app" "$stage/git-drive Files.app"
ln -s /Applications "$stage/Applications"
rm -f "$dmg"
hdiutil create -quiet -volname "git-drive Files" -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$dmg"

echo "==> signing the disk image"
if [ "$identity" = "-" ]; then
  codesign --force --sign - "$dmg"
else
  codesign --force --sign "$identity" --timestamp "$dmg"
fi

if [ "$notarize" = 1 ]; then
  echo "==> notarizing (Apple checks the image and everything in it)"
  xcrun notarytool submit "$dmg" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait
  xcrun stapler staple "$dmg"
fi

hdiutil verify -quiet "$dmg"
codesign --verify "$dmg"
echo "==> done ($( [ "$notarize" = 1 ] && echo "signed with $identity and notarized" || echo "signed ad hoc, not notarized" ))"
echo "$dmg"
