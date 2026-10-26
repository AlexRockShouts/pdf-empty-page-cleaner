#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

if ! command -v rustup >/dev/null 2>&1; then
  echo "Rust is required to build: https://rustup.rs" >&2
  exit 1
fi

rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
cargo build --release --target aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin

mkdir -p dist
lipo -create \
  target/aarch64-apple-darwin/release/pdf-cleaner \
  target/x86_64-apple-darwin/release/pdf-cleaner \
  -output dist/pdf-cleaner-macos-universal
codesign --force --sign - dist/pdf-cleaner-macos-universal

rm -rf dist/PDFCleaner.app
mkdir -p dist/PDFCleaner.app/Contents/MacOS dist/PDFCleaner.app/Contents/Resources
cp dist/pdf-cleaner-macos-universal dist/PDFCleaner.app/Contents/MacOS/PDFCleaner
cp assets/AppIcon.icns dist/PDFCleaner.app/Contents/Resources/AppIcon.icns

cat > dist/PDFCleaner.app/Contents/Info.plist <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDisplayName</key>
  <string>PDF Empty Page Cleaner</string>
  <key>CFBundleName</key>
  <string>PDFCleaner</string>
  <key>CFBundleExecutable</key>
  <string>PDFCleaner</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.alexrockshouts.pdfcleaner</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleShortVersionString</key>
  <string>1.0.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>10.15</string>
  <key>LSApplicationCategoryType</key>
  <string>public.app-category.utilities</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

codesign --force --sign - dist/PDFCleaner.app
rm -f dist/PDFCleaner-macos-universal.zip
ditto -c -k --keepParent dist/PDFCleaner.app dist/PDFCleaner-macos-universal.zip

echo "Built dist/PDFCleaner.app and dist/PDFCleaner-macos-universal.zip"
