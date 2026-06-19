#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

APP_DIR="$ROOT_DIR/src-tauri/target/release/bundle/macos/AiMaMi.app"
DMG_DIR="$ROOT_DIR/src-tauri/target/release/bundle/dmg"
DMG_PATH="$DMG_DIR/AiMaMi_1.0.0_aarch64.dmg"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/aimami-dmg.XXXXXX")"

cleanup() {
  hdiutil detach "/Volumes/AiMaMi" >/dev/null 2>&1 || true
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

pnpm tauri build

if [[ ! -d "$APP_DIR" ]]; then
  echo "Missing app bundle: $APP_DIR" >&2
  exit 1
fi

mkdir -p "$DMG_DIR"
rm -f "$DMG_PATH"
cp -R "$APP_DIR" "$WORK_DIR/AiMaMi.app"
ln -s /Applications "$WORK_DIR/Applications"

hdiutil create -volname "AiMaMi" -srcfolder "$WORK_DIR" -ov -format UDZO "$DMG_PATH"
hdiutil verify "$DMG_PATH"

echo "$DMG_PATH"
