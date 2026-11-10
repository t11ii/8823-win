#!/usr/bin/env bash
# Build a universal (Apple Silicon + Intel) 8823.app and zip it.
set -euo pipefail
cd "$(dirname "$0")/.."

rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
APP=dist/8823.app
rm -rf dist && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
lipo -create -output "$APP/Contents/MacOS/8823" \
  target/aarch64-apple-darwin/release/8823 \
  target/x86_64-apple-darwin/release/8823

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>8823</string>
  <key>CFBundleDisplayName</key><string>8823</string>
  <key>CFBundleExecutable</key><string>8823</string>
  <key>CFBundleIdentifier</key><string>io.github.t11ii.8823</string>
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
