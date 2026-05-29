#!/usr/bin/env bash
#
# Build the Rust core (libstow_core) and package it as StowCore.xcframework.
#
#   cargo build (aarch64) -> cbindgen header -> xcodebuild -create-xcframework
#
# Checksum-gated: re-runs only when the Rust sources / manifests / this script
# change, so incremental Xcode builds that touch only Swift don't rebuild Rust.
# Pass --force to rebuild unconditionally.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CRATE_DIR="$ROOT/rust/stow_core"
TARGET="aarch64-apple-darwin"   # Apple Silicon only (see plan)
PROFILE="release"
LIB_NAME="libstow_core.a"
LIB_PATH="$ROOT/target/$TARGET/$PROFILE/$LIB_NAME"

HEADERS_DIR="$ROOT/target/xcframework-headers"
HEADER_OUT="$HEADERS_DIR/stow_core.h"
MODULEMAP_SRC="$CRATE_DIR/include/module.modulemap"
PKG_INCLUDE="$ROOT/packages/StowCore/include"   # editor/reference copy
OUT="$ROOT/artifacts/StowCore.xcframework"

CHECKSUM_FILE="$ROOT/artifacts/.build-checksum"

force=0
[[ "${1:-}" == "--force" ]] && force=1

# --- checksum gate ---------------------------------------------------------
# Hash all inputs that affect the built artifact.
current_checksum() {
    {
        find "$CRATE_DIR/src" -type f -name '*.rs' -exec shasum {} +
        shasum "$CRATE_DIR/Cargo.toml" "$CRATE_DIR/cbindgen.toml" \
               "$MODULEMAP_SRC" "$ROOT/Cargo.toml" "${BASH_SOURCE[0]}"
    } 2>/dev/null | shasum | awk '{print $1}'
}

NEW_SUM="$(current_checksum)"
if [[ "$force" -eq 0 && -d "$OUT" && -f "$CHECKSUM_FILE" ]]; then
    if [[ "$(cat "$CHECKSUM_FILE")" == "$NEW_SUM" ]]; then
        echo "[stow] xcframework up to date (checksum match) — skipping. Use --force to rebuild."
        exit 0
    fi
fi

# --- build -----------------------------------------------------------------
echo "[stow] building Rust core for $TARGET ($PROFILE)…"
rustup target add "$TARGET" >/dev/null 2>&1 || true
# Build our own crate objects at the deployment floor.
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-14.0}"
# NOTE: the linker may warn "object file was built for newer macOS version (26.0)"
# for the PRECOMPILED std (core/compiler_builtins) — this toolchain's std targets
# the host OS. Harmless on macOS 26. To target macOS 14–25 for distribution,
# rebuild std from source: nightly + `-Z build-std` with MACOSX_DEPLOYMENT_TARGET=14.0.
cargo build --"$PROFILE" -p stow_core --target "$TARGET"

echo "[stow] generating C header via cbindgen…"
mkdir -p "$HEADERS_DIR" "$PKG_INCLUDE"
cbindgen --config "$CRATE_DIR/cbindgen.toml" --crate stow_core "$CRATE_DIR" --output "$HEADER_OUT"
cp "$MODULEMAP_SRC" "$HEADERS_DIR/module.modulemap"
cp "$HEADER_OUT" "$PKG_INCLUDE/stow_core.h"   # so editors resolve the symbols

echo "[stow] assembling xcframework…"
rm -rf "$OUT"
xcodebuild -create-xcframework \
    -library "$LIB_PATH" \
    -headers "$HEADERS_DIR" \
    -output "$OUT"

echo "$NEW_SUM" > "$CHECKSUM_FILE"
echo "[stow] done -> $OUT"
