#!/usr/bin/env bash
#
# Build a distributable macOS .app bundle and .dmg for Nyx.
#
# No external tooling required beyond what ships with Rust + macOS (sips,
# iconutil, hdiutil). The app's fonts/icons are embedded in the binary via
# rust-embed, so the bundle only needs the executable, an Info.plist and an
# app icon (.icns) generated from assets/nyx.png.
#
# Usage:
#   scripts/bundle-mac.sh                 # build for the host arch
#   scripts/bundle-mac.sh --universal     # build a universal (arm64 + x86_64) binary
#
# Output:
#   target/macos/Nyx.app
#   target/macos/Nyx-<version>.dmg
#
# Code signing / notarization is intentionally NOT done here (see README of the
# bundle step). The resulting .dmg runs locally; to share it with others you'd
# add `codesign` + `xcrun notarytool` afterwards.

set -euo pipefail

# --- Resolve repo root (script lives in <root>/scripts) -----------------------
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# --- Config -------------------------------------------------------------------
APP_NAME="Nyx"
BUNDLE_ID="com.vojir.nyx"
BIN_NAME="nyx"            # the [[bin]] name in crates/nyx/Cargo.toml
SOURCE_PNG="assets/nyx.png"
MIN_MACOS="11.0"

# Version from the workspace Cargo.toml ([workspace.package] version = "x.y.z").
VERSION="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' Cargo.toml | head -1)"
VERSION="${VERSION:-0.0.0}"

OUT_DIR="target/macos"
APP_DIR="$OUT_DIR/$APP_NAME.app"
CONTENTS="$APP_DIR/Contents"
DMG_PATH="$OUT_DIR/$APP_NAME-$VERSION.dmg"

# --- Xcode toolchain (Metal shaders are compiled at build time) ---------------
# GPUI's macOS backend needs full Xcode, not just the Command Line Tools.
if [ -z "${DEVELOPER_DIR:-}" ] && [ -d "/Applications/Xcode.app/Contents/Developer" ]; then
  export DEVELOPER_DIR="/Applications/Xcode.app/Contents/Developer"
fi

# --- Parse args ---------------------------------------------------------------
UNIVERSAL=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

echo "==> Building Nyx $VERSION (release)"

# --- 1. Compile the release binary -------------------------------------------
if [ "$UNIVERSAL" -eq 1 ]; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
  cargo build --release -p "$BIN_NAME" --target aarch64-apple-darwin
  cargo build --release -p "$BIN_NAME" --target x86_64-apple-darwin
  BIN_SRC="$OUT_DIR/$BIN_NAME-universal"
  mkdir -p "$OUT_DIR"
  lipo -create -output "$BIN_SRC" \
    "target/aarch64-apple-darwin/release/$BIN_NAME" \
    "target/x86_64-apple-darwin/release/$BIN_NAME"
else
  cargo build --release -p "$BIN_NAME"
  BIN_SRC="target/release/$BIN_NAME"
fi

# --- 2. Generate the .icns from assets/nyx.png -------------------------------
echo "==> Generating app icon from $SOURCE_PNG"
ICONSET="$(mktemp -d)/$APP_NAME.iconset"
mkdir -p "$ICONSET"
# Normalize to a clean 1024 base first (source is 1028x1028).
BASE_PNG="$(mktemp -d)/base.png"
sips -z 1024 1024 "$SOURCE_PNG" --out "$BASE_PNG" >/dev/null
for spec in "16:16x16" "32:16x16@2x" "32:32x32" "64:32x32@2x" \
            "128:128x128" "256:128x128@2x" "256:256x256" "512:256x256@2x" \
            "512:512x512" "1024:512x512@2x"; do
  px="${spec%%:*}"; label="${spec##*:}"
  sips -z "$px" "$px" "$BASE_PNG" --out "$ICONSET/icon_$label.png" >/dev/null
done

# --- 3. Assemble the .app bundle ---------------------------------------------
echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$CONTENTS/MacOS" "$CONTENTS/Resources"
cp "$BIN_SRC" "$CONTENTS/MacOS/$BIN_NAME"
chmod +x "$CONTENTS/MacOS/$BIN_NAME"
iconutil -c icns "$ICONSET" -o "$CONTENTS/Resources/$APP_NAME.icns"

cat > "$CONTENTS/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>            <string>$APP_NAME</string>
  <key>CFBundleDisplayName</key>     <string>$APP_NAME</string>
  <key>CFBundleIdentifier</key>      <string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key>      <string>$BIN_NAME</string>
  <key>CFBundleIconFile</key>        <string>$APP_NAME</string>
  <key>CFBundleVersion</key>         <string>$VERSION</string>
  <key>CFBundleShortVersionString</key> <string>$VERSION</string>
  <key>CFBundlePackageType</key>     <string>APPL</string>
  <key>LSMinimumSystemVersion</key>  <string>$MIN_MACOS</string>
  <key>NSHighResolutionCapable</key> <true/>
  <key>LSApplicationCategoryType</key> <string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

# Ad-hoc sign so the bundle launches without "is damaged" on the build machine.
# (Not a Developer ID signature — fine for local use, not for distribution.)
codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1 || \
  echo "    (codesign skipped — bundle is unsigned)"

# --- 4. Build the .dmg --------------------------------------------------------
echo "==> Creating $DMG_PATH"
STAGE="$(mktemp -d)"
cp -R "$APP_DIR" "$STAGE/"
ln -s /Applications "$STAGE/Applications"   # drag-to-install target
rm -f "$DMG_PATH"
hdiutil create \
  -volname "$APP_NAME $VERSION" \
  -srcfolder "$STAGE" \
  -ov -format UDZO \
  "$DMG_PATH" >/dev/null

echo ""
echo "Done."
echo "  App: $APP_DIR"
echo "  DMG: $DMG_PATH"
