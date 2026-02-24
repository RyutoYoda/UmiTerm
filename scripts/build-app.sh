#!/bin/bash
set -e

VERSION="${1:-0.1.0}"
APP_NAME="UmiTerm"
BUNDLE_ID="com.ryutoyoda.umiterm"

echo "Building ${APP_NAME} v${VERSION}..."

# Release build
cargo build --release

# Create .app bundle structure
APP_DIR="${APP_NAME}.app"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
mkdir -p "$APP_DIR/Contents/Resources"

# Copy binary
cp target/release/umiterm "$APP_DIR/Contents/MacOS/"

# Copy icon
cp AppIcon.icns "$APP_DIR/Contents/Resources/"

# Create Info.plist
cat > "$APP_DIR/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleVersion</key>
    <string>${VERSION}</string>
    <key>CFBundleShortVersionString</key>
    <string>${VERSION}</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>umiterm</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
</dict>
</plist>
EOF

# Create zip for distribution
ZIP_NAME="UmiTerm-v${VERSION}-macos-arm64.zip"
rm -f "$ZIP_NAME"
ditto -c -k --keepParent "$APP_DIR" "$ZIP_NAME"

echo ""
echo "✓ Created: ${APP_DIR}"
echo "✓ Created: ${ZIP_NAME}"
echo ""
echo "To install: Unzip and drag UmiTerm.app to /Applications"
