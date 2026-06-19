#!/usr/bin/env bash
# pack-release.sh — build, package, and locally install usbipd-rs.
#
# What it does:
#   1. Build a warning-clean release binary (delegates to ./build.sh).
#   2. Package the binary + README + LICENSE into a versioned archive under
#      dist/  (zip on Windows, tar.gz on macOS/Linux) — same layout the CI
#      release uses, so the local artifact matches a published one.
#   3. Install the binary into ~/.local/bin, overwriting any older copy there.
#      On macOS, also copy it into ~/.cargo/bin (typically already on PATH).
#
# This is the LOCAL counterpart to the tag-triggered GitHub release: use it to
# put a freshly built usbipd-rs on your own PATH without cutting a real release.
#
# Usage:
#   ./pack-release.sh
set -euo pipefail

cd "$(dirname "$0")"

# Detect OS → binary name, archive format, and a CI-style platform label.
arch="$(uname -m)"
case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) os=windows; bin=usbipd-rs.exe; ext=zip ;;
  Darwin)                   os=macos;   bin=usbipd-rs;     ext=tar.gz ;;
  *)                        os=linux;   bin=usbipd-rs;     ext=tar.gz ;;
esac

# ── 1. build ────────────────────────────────────────────────────────────
./build.sh

version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"
src="target/release/$bin"
if [ ! -f "$src" ]; then
  echo "error: $src not found — did the build fail?" >&2
  exit 1
fi

# ── 2. package into dist/ ───────────────────────────────────────────────
stage="usbipd-rs-$version-$os-$arch"
rm -rf "dist/$stage"
mkdir -p "dist/$stage"
cp "$src" "dist/$stage/"
[ -f README.md ] && cp README.md "dist/$stage/"
[ -f LICENSE ]   && cp LICENSE   "dist/$stage/"

# Bundle the optional chip-definition templates next to the binary so
# `--mcu-alive-native` finds them via its exe-relative `etc/chips` search path.
if [ -d etc/chips ]; then
  mkdir -p "dist/$stage/etc/chips"
  cp etc/chips/*.chip      "dist/$stage/etc/chips/" 2>/dev/null || true
  cp etc/chips/README.md   "dist/$stage/etc/chips/" 2>/dev/null || true
fi

archive="dist/$stage.$ext"
rm -f "$archive"

# Build the archive with whatever tool works. Each candidate is guarded by its
# own `command -v ... && run` so a missing OR broken tool falls through to the
# next instead of aborting the script (PowerShell is most reliable on Windows).
pack_archive() (
  cd dist
  if [ "$ext" = zip ]; then
    if command -v powershell >/dev/null 2>&1 && \
       powershell -NoProfile -Command \
         "Compress-Archive -Force -Path '$stage' -DestinationPath '$stage.zip'" >/dev/null 2>&1
    then return 0; fi
    if command -v 7z  >/dev/null 2>&1 && 7z a -tzip "$stage.zip" "$stage" >/dev/null 2>&1; then return 0; fi
    if command -v zip >/dev/null 2>&1 && zip -rq "$stage.zip" "$stage"           >/dev/null 2>&1; then return 0; fi
    return 1
  else
    tar czf "$stage.tar.gz" "$stage"
  fi
)

if pack_archive; then
  echo "Packaged: $archive"
else
  echo "warning: no working archive tool (powershell/7z/zip/tar) — skipped $archive." >&2
fi
# Keep only the archive; drop the unpacked staging dir.
rm -rf "dist/$stage"

# ── 3. install into ~/.local/bin (overwrite old) ────────────────────────
dest="$HOME/.local/bin"
mkdir -p "$dest"
cp -f "$src" "$dest/$bin"
chmod +x "$dest/$bin" 2>/dev/null || true
echo "Installed: $dest/$bin  (usbipd-rs $version)"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "note: $dest is not on your PATH — add it to use 'usbipd-rs' directly." >&2 ;;
esac

# On macOS, also drop a copy into ~/.cargo/bin (usually already on PATH for
# Rust users), so `usbipd-rs` is runnable without touching ~/.local/bin.
if [ "$os" = macos ]; then
  cargo_dest="${CARGO_HOME:-$HOME/.cargo}/bin"
  mkdir -p "$cargo_dest"
  cp -f "$src" "$cargo_dest/$bin"
  chmod +x "$cargo_dest/$bin" 2>/dev/null || true
  echo "Installed: $cargo_dest/$bin  (usbipd-rs $version)"
fi
