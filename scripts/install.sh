#!/bin/sh
# ultra-media-remote installer for macOS arm64:
#   curl -fsSL https://github.com/michael-berardi/ultra-media-remote/releases/latest/download/install.sh | sh
# Downloads the signed release, checks its SHA-256, installs into
# ~/.local/share/ultra-media-remote/<version> and puts a shim in ~/.local/bin.
# Refuses to replace a binary it did not install.
#
# Environment: ULTRA_MEDIA_REMOTE_HOME (default ~/.local/share/ultra-media-remote),
# ULTRA_MEDIA_REMOTE_BIN_DIR (default ~/.local/bin), ULTRA_MEDIA_REMOTE_VERSION
# (default: latest), ULTRA_MEDIA_REMOTE_RELEASE_BASE (download base URL, used by tests).
set -eu

REPO="michael-berardi/ultra-media-remote"
ASSET="ultra-media-remote-macos-arm64.tar.gz"
MARKER=".ultra-media-remote-install"
SHIM_MARKER="# ultra-media-remote installer shim"
HOME_DIR="${ULTRA_MEDIA_REMOTE_HOME:-$HOME/.local/share/ultra-media-remote}"
case "$HOME_DIR" in /*) ;; *) HOME_DIR="$(pwd)/$HOME_DIR" ;; esac
BIN_DIR="${ULTRA_MEDIA_REMOTE_BIN_DIR:-$HOME/.local/bin}"
case "$BIN_DIR" in /*) ;; *) BIN_DIR="$(pwd)/$BIN_DIR" ;; esac
if [ -n "${ULTRA_MEDIA_REMOTE_RELEASE_BASE:-}" ]; then BASE="$ULTRA_MEDIA_REMOTE_RELEASE_BASE"
elif [ -n "${ULTRA_MEDIA_REMOTE_VERSION:-}" ]; then BASE="https://github.com/$REPO/releases/download/v${ULTRA_MEDIA_REMOTE_VERSION#v}"
else BASE="https://github.com/$REPO/releases/latest/download"; fi

say() { printf '%s\n' "$*"; }
die() { printf 'ultra-media-remote install: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
Darwin) ;;
*) die "this installer ships a signed macOS binary only (uname -s=$(uname -s)); on Linux or Windows use the crate with 'cargo add ultra-media-remote --git https://github.com/$REPO' instead" ;;
esac
case "$(uname -m)" in
arm64) ;;
*) [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ] || die "the release binary is arm64 only; this Mac reports $(uname -m)" ;;
esac

if command -v curl >/dev/null 2>&1; then fetch() { curl -fsSL --retry 3 -o "$2" "$1" || die "download failed: $1"; }
elif command -v wget >/dev/null 2>&1; then fetch() { wget -q -O "$2" "$1" || die "download failed: $1"; }
else die "needs curl or wget"; fi
command -v tar >/dev/null 2>&1 || die "needs tar"

if command -v sha256sum >/dev/null 2>&1; then sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "needs sha256sum or shasum"; fi

main() {
TMP=$(mktemp -d "${TMPDIR:-/tmp}/ultra-media-remote-install.XXXXXX") || die "cannot create a temporary folder"
trap 'rm -rf "$TMP"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# expect <file> <name> <sums-file>: the file's SHA-256 must match <name> in SHA256SUMS.
expect() {
  want=$(awk -v n="$2" '{ f=$2; sub(/^\*/, "", f); if (f == n) print $1 }' "$3")
  [ -n "$want" ] || die "no checksum for $2 in $(basename "$3")"
  [ "$(sha256 "$1")" = "$want" ] || die "checksum mismatch for $2; nothing was installed"
}

# swap_link <target> <link>: rename a fresh link over the old one.
swap_link() {
  rm -f "$2.tmp.$$"
  ln -s "$1" "$2.tmp.$$"
  if mv -fT "$2.tmp.$$" "$2" 2>/dev/null || mv -fh "$2.tmp.$$" "$2" 2>/dev/null; then return 0; fi
  rm -f "$2" && mv "$2.tmp.$$" "$2" || { rm -f "$2.tmp.$$"; die "cannot update the link $2"; }
}

say "Downloading ultra-media-remote..."
fetch "$BASE/$ASSET" "$TMP/$ASSET"
fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS"
expect "$TMP/$ASSET" "$ASSET" "$TMP/SHA256SUMS"
mkdir "$TMP/src"
tar -xzf "$TMP/$ASSET" -C "$TMP/src" || die "cannot unpack the download"
BIN="$TMP/src/ultra-media-remote.app/Contents/MacOS/ultra-media-remote"
ADAPTER="$TMP/src/ultra-media-remote.app/Contents/Resources/mediaremote"
[ -x "$BIN" ] && [ -f "$TMP/src/VERSION" ] &&
  [ -f "$ADAPTER/mediaremote-adapter.pl" ] &&
  [ -d "$ADAPTER/MediaRemoteAdapter.framework" ] &&
  [ -f "$TMP/src/LICENSE" ] && [ -f "$TMP/src/THIRD_PARTY_NOTICES.md" ] ||
  die "the download is not an ultra-media-remote release"

VERSION=$(tr -d '[:space:]' < "$TMP/src/VERSION")
printf '%s' "$VERSION" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "bad version in the release: '$VERSION'"
got=$("$BIN" --version 2>&1 </dev/null) ||
  die "the downloaded binary does not start: $got"
case "$got" in "ultra-media-remote $VERSION"*) ;; *) die "the download reports '$got', not $VERSION" ;; esac

mkdir -p "$HOME_DIR" "$BIN_DIR"
chmod 700 "$HOME_DIR" 2>/dev/null || true
REL="$HOME_DIR/$VERSION"
CURRENT="$HOME_DIR/current"
SHIM="$BIN_DIR/ultra-media-remote"

if [ -d "$REL" ]; then
  [ -f "$REL/$MARKER" ] || die "$REL exists but was not installed by this installer; refusing to clobber it"
  say "ultra-media-remote $VERSION is already installed; relinking."
else
  if [ -e "$REL" ] || [ -L "$REL" ]; then die "$REL exists but is not an install folder; refusing to clobber it"; fi
  say "Installing ultra-media-remote $VERSION..."
  rm -rf "$REL.tmp.$$"
  cp -R "$TMP/src" "$REL.tmp.$$" || die "cannot copy the release into place"
  printf 'installer=%s\nversion=%s\n' "$ASSET" "$VERSION" > "$REL.tmp.$$/$MARKER"
  got=$("$REL.tmp.$$/ultra-media-remote.app/Contents/MacOS/ultra-media-remote" --version 2>&1 </dev/null) ||
    { rm -rf "$REL.tmp.$$"; die "the installed binary does not start: $got"; }
  case "$got" in "ultra-media-remote $VERSION"*) ;; *) rm -rf "$REL.tmp.$$"; die "the installed copy reports '$got'" ;; esac
  mv "$REL.tmp.$$" "$REL" || { rm -rf "$REL.tmp.$$"; die "cannot move the release into $REL"; }
fi

if [ -e "$CURRENT" ] || [ -L "$CURRENT" ]; then
  [ -L "$CURRENT" ] || die "$CURRENT exists but is not an installer link; refusing to clobber it"
fi
swap_link "$REL" "$CURRENT"

if [ -e "$SHIM" ] || [ -L "$SHIM" ]; then
  if [ -L "$SHIM" ] || ! grep -q "$SHIM_MARKER" "$SHIM" 2>/dev/null; then
    die "$SHIM exists and is not an ultra-media-remote installer shim; refusing to replace it"
  fi
fi
cat > "$SHIM.tmp.$$" <<WRAP
#!/bin/sh
$SHIM_MARKER
exec "$REL/ultra-media-remote.app/Contents/MacOS/ultra-media-remote" "\$@"
WRAP
chmod 755 "$SHIM.tmp.$$"
mv -f "$SHIM.tmp.$$" "$SHIM" || { rm -f "$SHIM.tmp.$$"; die "cannot install the shim $SHIM"; }

# The stub binary is a self-check: it must start from the installed location.
installed=$("$SHIM" --version 2>&1 </dev/null) || die "the installed shim does not start: $installed"
case "$installed" in "ultra-media-remote $VERSION"*) ;; *) die "the shim reports '$installed'" ;; esac

say "ultra-media-remote $VERSION installed ($SHIM)."
case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Add $BIN_DIR to your PATH:  export PATH=\"$BIN_DIR:\$PATH\"" ;; esac
}

main "$@"
