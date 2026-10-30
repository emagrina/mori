#!/bin/sh
set -eu
ROOT="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
OUT="$ROOT/src-tauri/target/store-icon"
mkdir -p "$OUT"
xcrun actool "$ROOT/src-tauri/icons/MoriAdaptive.icon" \
  --compile "$OUT" --platform macosx --minimum-deployment-target 12.0 \
  --app-icon MoriAdaptive --target-device mac \
  --output-partial-info-plist "$OUT/Info.plist" --output-format human-readable-text
test -s "$OUT/Assets.car"
test -s "$OUT/MoriAdaptive.icns"
