#!/bin/bash
# Manual archive integration: embed before signing the enclosing app, never --deep sign.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="${1:?Usage: embed-decoder.sh /path/Mori.app [signing identity]}"
[[ -d "$APP/Contents/MacOS" ]] || { echo 'Expected an existing app bundle' >&2; exit 1; }
"$ROOT/xpc/prepare-decoder.sh" "${2:-8DDF5A00D0405A3BB6EB3F7DC38E5F0862AFB2FA}"
mkdir -p "$APP/Contents/XPCServices"
# ditto copies the sealed bundle, including its signature and minimal entitlements.
ditto "$ROOT/target/store-decoder/app.mori.viewer.decoder.xpc" "$APP/Contents/XPCServices/app.mori.viewer.decoder.xpc"
printf 'Embedded decoder. Sign the host app next, then run: python3 %s/xpc/verify-decoder.py --app "%s"\n' "$ROOT" "$APP"
