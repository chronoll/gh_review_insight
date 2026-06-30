#!/usr/bin/env bash
#
# Build a macOS .app bundle for gh-review-insight.
#
#   ./scripts/bundle-macos.sh            # build + assemble .app under target/
#   ./scripts/bundle-macos.sh --install  # also copy to /Applications
#   ./scripts/bundle-macos.sh --login    # also install + add as a login item
#
set -euo pipefail
cd "$(dirname "$0")/.."

APP_NAME="gh-review-insight"
BIN="target/release/$APP_NAME"
OUT_DIR="target/release/macos"
APP="$OUT_DIR/$APP_NAME.app"

INSTALL=false
LOGIN=false
for arg in "$@"; do
  case "$arg" in
    --install) INSTALL=true ;;
    --login) INSTALL=true; LOGIN=true ;;  # login item must live in /Applications
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

echo "==> building release binary"
cargo build --release

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$APP_NAME"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>${APP_NAME}</string>
  <key>CFBundleDisplayName</key><string>gh-review-insight</string>
  <key>CFBundleIdentifier</key><string>com.chronoll.gh-review-insight</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleExecutable</key><string>${APP_NAME}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Optional icon: drop an icon.icns at assets/icon.icns to embed it.
if [ -f "assets/icon.icns" ]; then
  cp assets/icon.icns "$APP/Contents/Resources/icon.icns"
  /usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string icon" \
    "$APP/Contents/Info.plist" >/dev/null 2>&1 || true
fi

echo "==> ad-hoc codesign"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "   (codesign skipped)"

echo "built: $APP"

if $INSTALL; then
  echo "==> installing to /Applications"
  rm -rf "/Applications/$APP_NAME.app"
  cp -R "$APP" "/Applications/$APP_NAME.app"
  echo "installed: /Applications/$APP_NAME.app"
fi

if $LOGIN; then
  echo "==> adding login item"
  osascript -e "tell application \"System Events\" to make login item at end with properties {path:\"/Applications/${APP_NAME}.app\", hidden:false}" >/dev/null
  echo "login item added (System Settings > General > Login Items で確認できます)"
fi
