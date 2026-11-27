#!/bin/sh
# Builds the release assets for the current commit into <out> (default dist/release):
#   ultra-media-remote-macos-arm64.tar.gz   Developer ID signed, notarized, stapled CLI + adapter + notices
#   install.sh                              the one-line installer
#   SHA256SUMS                              checksums of both files above
# Usage: ULTRA_MEDIA_REMOTE_SIGNING_IDENTITY="Developer ID Application: ..." \
#        NOTARY_PROFILE=<notarytool keychain profile> scripts/build-release.sh [out-dir]
# No signing identity or notary profile is stored in the repository.
set -eu
cd "$(dirname "$0")/.."
die() { printf 'build-release: %s\n' "$*" >&2; exit 1; }

[ -z "$(git status --porcelain --untracked-files=no)" ] ||
  die "the working tree has changes; build from a clean checkout"
[ "$(uname -s)" = Darwin ] || die "the release binary is macOS only"
case "$(uname -m)" in arm64) ;; *) die "build the arm64 release on Apple silicon" ;; esac
[ "$(rustc -vV | sed -n 's/^host: //p')" = aarch64-apple-darwin ] ||
  die "rustc host must be aarch64-apple-darwin"
command -v cargo >/dev/null || die "cargo is unavailable"
command -v cmake >/dev/null || die "cmake is unavailable"
command -v codesign >/dev/null || die "codesign is unavailable"
command -v ditto >/dev/null || die "ditto is unavailable"
command -v shasum >/dev/null || die "shasum is unavailable"
command -v realpath >/dev/null || die "realpath is unavailable"

IDENTITY="${ULTRA_MEDIA_REMOTE_SIGNING_IDENTITY:?Set ULTRA_MEDIA_REMOTE_SIGNING_IDENTITY to a Developer ID Application identity}"
NOTARY_PROFILE="${NOTARY_PROFILE:?Set NOTARY_PROFILE to a notarytool keychain profile}"
case "$IDENTITY" in "Developer ID Application:"*) ;; *) die "the signing identity must be a Developer ID Application identity" ;; esac
TEAM=$(printf '%s' "$IDENTITY" | sed -n 's/.*(\([[:alnum:]]\{10\}\))$/\1/p')
[ -n "$TEAM" ] || die "cannot read a 10-character Team ID from the signing identity"

# Resolve the output path before any destructive removal. Symlinks in an
# existing prefix are resolved; a missing final component is kept lexically.
canonicalize() {
  p=$1
  case "$p" in /*) ;; *) p="$(pwd -P)/$p" ;; esac
  suffix=""
  while [ ! -e "$p" ] && [ ! -L "$p" ]; do
    base=${p##*/}
    parent=${p%/*}
    [ -n "$base" ] && [ "$base" != "$p" ] || break
    suffix="/$base$suffix"
    p=$parent
    [ -n "$p" ] || p=/
  done
  resolved=$(realpath "$p") || die "cannot resolve $1"
  printf '%s%s\n' "$resolved" "$suffix"
}

OUT=$(canonicalize "${1:-dist/release}")
ROOT=$(pwd -P)
HOME_REAL=$(canonicalize "$HOME")
[ "$OUT" != "$ROOT" ] || die "refusing to delete the checkout: $OUT"
[ "$OUT" != "/" ] || die "refusing to delete /"
[ "$OUT" != "$HOME_REAL" ] || die "refusing to delete the home directory: $OUT"
case "$OUT" in
  "$ROOT"/dist/?*) ;;
  *) die "refusing to delete $OUT: the output must resolve under $ROOT/dist/" ;;
esac
VERSION=$(sed -n 's/^version = "\([^"]*\)".*/\1/p' Cargo.toml | head -n 1)
[ -n "$VERSION" ] || die "cannot read the crate version"
ASSET="ultra-media-remote-macos-arm64.tar.gz"
BUILD="$(pwd)/.release-build"
STAGE="$BUILD/stage"
APP="$STAGE/ultra-media-remote.app"
CONTENTS="$APP/Contents"
COMMIT=$(git rev-parse HEAD)

rm -rf "$BUILD" "$OUT"
mkdir -p "$BUILD/adapter" "$CONTENTS/MacOS" "$CONTENTS/Resources/mediaremote" "$OUT"

echo "build-release: building ultra-media-remote $VERSION ($COMMIT)"
cargo build --locked --release --bin ultra-media-remote -j 4
cp target/release/ultra-media-remote "$CONTENTS/MacOS/ultra-media-remote"
chmod 755 "$CONTENTS/MacOS/ultra-media-remote"

echo "build-release: building the vendored MediaRemoteAdapter framework"
cmake -S third_party/mediaremote-adapter -B "$BUILD/adapter" \
  -DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=arm64
cmake --build "$BUILD/adapter" --target MediaRemoteAdapter --parallel 4
FRAMEWORK="$BUILD/adapter/MediaRemoteAdapter.framework"
[ -d "$FRAMEWORK" ] || die "the adapter framework was not built"
cp -R "$FRAMEWORK" "$CONTENTS/Resources/mediaremote/"
cp third_party/mediaremote-adapter/bin/mediaremote-adapter.pl "$CONTENTS/Resources/mediaremote/"
chmod 755 "$CONTENTS/Resources/mediaremote/mediaremote-adapter.pl"
cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>ultra-media-remote</string>
  <key>CFBundleIdentifier</key><string>com.paretocybernetics.ultra-media-remote</string>
  <key>CFBundleExecutable</key><string>ultra-media-remote</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
PLIST
cp LICENSE THIRD_PARTY_NOTICES.md "$STAGE/"
printf '%s\n' "$VERSION" > "$STAGE/VERSION"

echo "build-release: signing with Developer ID and the hardened runtime"
# Sign the nested framework explicitly: codesign --deep does not reliably
# re-sign an ad-hoc CMake framework with a secure timestamp.
FW="$CONTENTS/Resources/mediaremote/MediaRemoteAdapter.framework"
codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$FW"
codesign --verify --strict --verbose=2 --deep "$FW"
codesign --force --deep --options runtime --timestamp --sign "$IDENTITY" "$APP"
codesign --verify --strict --verbose=2 --deep "$APP"
if [ -n "$TEAM" ]; then
  codesign --verify --strict --verbose=2 \
    -R "=identifier \"com.paretocybernetics.ultra-media-remote\" and anchor apple generic and certificate leaf[subject.OU] = $TEAM" \
    "$APP"
fi
"$CONTENTS/MacOS/ultra-media-remote" --version >/dev/null

echo "build-release: notarizing and stapling the zip"
ZIP="$OUT/ultra-media-remote-macos-arm64-notarization.zip"
ditto -c -k --keepParent "$STAGE" "$ZIP"
xcrun notarytool submit "$ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
xcrun stapler staple "$APP"
xcrun stapler validate "$APP"
spctl -a -t exec -vv "$APP"
rm -f "$ZIP"

( cd "$STAGE" && COPYFILE_DISABLE=1 tar -czf "$OUT/$ASSET" \
    ultra-media-remote.app VERSION LICENSE THIRD_PARTY_NOTICES.md )
cp scripts/install.sh "$OUT/install.sh"
chmod 755 "$OUT/install.sh"
if command -v sha256sum >/dev/null 2>&1; then
  ( cd "$OUT" && sha256sum "$ASSET" install.sh > SHA256SUMS )
else
  ( cd "$OUT" && shasum -a 256 "$ASSET" install.sh > SHA256SUMS )
fi
echo "build-release: built $OUT for $COMMIT"
( cd "$OUT" && ls -lh && cat SHA256SUMS )
