#!/bin/bash
# Run BEFORE building the Store app. The app bundler copies this signed nested bundle.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
IDENTITY="${1:-8DDF5A00D0405A3BB6EB3F7DC38E5F0862AFB2FA}"
export MACOSX_DEPLOYMENT_TARGET=12.0
for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --locked --release --manifest-path "$ROOT/decoder/Cargo.toml" --target "$arch"
done
mkdir -p "$ROOT/target/store-decoder"
STAGING="$(mktemp -d "$ROOT/target/store-decoder/staging.XXXXXX")"
trap 'rm -rf "$STAGING"' EXIT
DEST="$STAGING/app.mori.viewer.decoder.xpc"
mkdir -p "$DEST/Contents/MacOS"
cp "$ROOT/xpc/Info.plist" "$DEST/Contents/Info.plist"
lipo -create "$ROOT/decoder/target/aarch64-apple-darwin/release/mori-decoder" "$ROOT/decoder/target/x86_64-apple-darwin/release/mori-decoder" -output "$DEST/Contents/MacOS/mori-decoder"
codesign --force --options runtime --timestamp --sign "$IDENTITY" --entitlements "$ROOT/xpc/Entitlements.plist" "$DEST"
# Destination is generated output only. Source and existing signed app bundles are untouched.
python3 - "$DEST" "$ROOT/target/store-decoder/app.mori.viewer.decoder.xpc" <<'PY'
import shutil,sys
from pathlib import Path
source,dest=map(Path,sys.argv[1:])
if dest.exists():shutil.rmtree(dest)
shutil.move(source,dest)
PY
python3 "$ROOT/xpc/verify-decoder.py" --record
