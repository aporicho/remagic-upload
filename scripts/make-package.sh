#!/usr/bin/env bash
set -euo pipefail
umask 022

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET=${TARGET:-aarch64-unknown-linux-gnu}
PACKAGE_TARGET=${PACKAGE_TARGET:-universal_aarch64}
BIN=${REMAGIC_UPLOAD_BIN:-$ROOT/target/$TARGET/release/remagic-upload}
OUT=${OUT_DIR:-$ROOT/dist}
VERSION=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)
BUNDLE=$OUT/remagic-upload-$VERSION-$PACKAGE_TARGET
ARCHIVE=$BUNDLE.tar.gz

[[ -x "$BIN" ]] || { echo "missing release binary: $BIN" >&2; exit 1; }
grep -qx "version = \"$VERSION\"" "$ROOT/manifests/upload.toml"
rm -rf "$BUNDLE" "$ARCHIVE"
mkdir -p "$BUNDLE/payload/bin"
install -m 0644 "$ROOT/manifests/upload.toml" "$BUNDLE/manifest.toml"
install -m 0755 "$BIN" "$BUNDLE/payload/bin/remagic-upload"
python3 "$ROOT/scripts/remagic-bundle.py" create "$BUNDLE" \
    --app-id upload --package remagic-upload --version "$VERSION"
python3 "$ROOT/scripts/remagic-bundle.py" verify "$BUNDLE" \
    --app-id upload --package remagic-upload --version "$VERSION"
tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -C "$BUNDLE" -cf - bundle.json manifest.toml payload | gzip -n >"$ARCHIVE"
printf '%s\n' "$ARCHIVE"
sha256sum "$ARCHIVE"
