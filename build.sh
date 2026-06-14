#!/usr/bin/env bash
# build.sh — release build for usbipd-rs.
#
# Mirrors CI (.github/workflows/release.yml): builds with RUSTFLAGS="-D warnings"
# so any compiler warning fails the build locally, exactly as it would on GitHub
# Actions. Cargo.lock is gitignored, so (like CI) we resolve deps fresh — no
# --locked.
#
# Usage:
#   ./build.sh                          # cargo build --release (host target)
#   ./build.sh --target <triple>        # cross-build for a specific target
#   ./build.sh x86_64-pc-windows-msvc   # shorthand: a bare triple means --target
#
# Override the warning policy if you really need to:
#   RUSTFLAGS="" ./build.sh
set -euo pipefail

cd "$(dirname "$0")"

# Match the CI deny-warnings policy unless the caller overrides RUSTFLAGS.
export RUSTFLAGS="${RUSTFLAGS:--D warnings}"

target_args=()
case "${1:-}" in
  "")        ;;
  --target)  target_args=(--target "${2:?--target needs a triple}") ;;
  -*)        echo "build.sh: unknown flag '$1'" >&2; exit 2 ;;
  *)         target_args=(--target "$1") ;;
esac

version="$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/')"

echo ">> usbipd-rs $version"
echo ">> cargo build --release ${target_args[*]-}  (RUSTFLAGS='$RUSTFLAGS')"
cargo build --release "${target_args[@]}"

# Report where the binary landed.
if [ "${#target_args[@]}" -gt 0 ]; then
  out_dir="target/${target_args[1]}/release"
else
  out_dir="target/release"
fi
for name in usbipd-rs usbipd-rs.exe; do
  if [ -f "$out_dir/$name" ]; then
    echo
    echo "Build OK — $out_dir/$name"
    exit 0
  fi
done

echo
echo "Build finished (binary not found under $out_dir — check the cargo output above)."
