#!/usr/bin/env bash
# Build 8823.app and zip it.
#
# Apple Silicon only by default, which keeps the app half the size of a universal one.
# For a universal (Apple Silicon + Intel) app:  ARCHS="aarch64 x86_64" ./scripts/bundle-mac.sh
set -euo pipefail
cd "$(dirname "$0")/.."

ARCHS=${ARCHS:-aarch64}
BINS=()
for arch in $ARCHS; do
  rustup target add "$arch-apple-darwin"
  cargo build --release --target "$arch-apple-darwin"
  BINS+=("target/$arch-apple-darwin/release/8823")
done

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=dist/8823.app
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [ "${#BINS[@]}" -eq 1 ]; then
  cp "${BINS[0]}" "$APP/Contents/MacOS/8823"
else
  lipo -create -output "$APP/Contents/MacOS/8823" "${BINS[@]}"
fi

cp assets/icon/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>8823</string>
  <key>CFBundleDisplayName</key><string>8823</string>
  <key>CFBundleExecutable</key><string>8823</string>
  <key>CFBundleIdentifier</key><string>io.github.t11ii.8823</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleDevelopmentRegion</key><string>ja</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
</dict>
</plist>
PLIST

# Ad-hoc signature (no Apple Developer ID): enough for Apple Silicon to run it.
codesign --force --deep --sign - "$APP"
ditto -c -k --keepParent "$APP" dist/8823-mac.zip
echo "Built dist/8823-mac.zip"
