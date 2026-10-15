#!/bin/bash
#
# Build script for the NPlank macOS application.
# Creates an unsigned DMG for distribution (adapted from the SotF
# scripts/build-dmg-sotf.sh flow).
#
# Usage:
#   ./scripts/build-macos.sh              # Release build + NPlank.app + DMG
#   ./scripts/build-macos.sh --debug      # Faster debug build (no DMG rename)
#   ./scripts/build-macos.sh --binary <path>
#                                         # Package an already-built nplank binary
#   ./scripts/build-macos.sh --clean      # Clean the staging dir before building
#
# Signing:
#   Without your Developer ID certificate the DMG is unsigned and Gatekeeper
#   will refuse it on other machines. If DEVELOPER_ID is set, payload code and
#   the DMG container are signed during the build (same split as SotF, where
#   container notarization stays a separate step). To finish a release build,
#   notarize afterwards, e.g.:
#     export DEVELOPER_ID='Developer ID Application: Your Name (TEAMID)'
#     ./scripts/build-macos.sh
#     xcrun notarytool submit dist/NPlank-*.dmg --apple-id you@example.com \
#       --keychain-profile <profile> --wait
#     xcrun stapler staple dist/NPlank-*.dmg
#
# Prerequisites:
#   - Xcode Command Line Tools (hdiutil, codesign)
#   - Rust toolchain from rust-toolchain.toml
#   - create-dmg (optional, for a styled DMG): brew install create-dmg
#

set -euo pipefail

APP_NAME="NPlank"
BINARY_NAME="nplank"
DAEMON_BINARY="nplankd"
BUNDLE_ID="org.spinorama.nplank"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOGO_PNG="$PROJECT_ROOT/plank/assets/logo.png"

VERSION=$(grep -m1 '^version = ' "$PROJECT_ROOT/Cargo.toml" | sed -E 's/version = "(.*)"/\1/')
if [ -z "$VERSION" ]; then
    echo "ERROR: Could not extract version from Cargo.toml"
    exit 1
fi

BUILD_DIR="$PROJECT_ROOT/target/release"
DMG_DIR="$PROJECT_ROOT/target/dmg"
APP_BUNDLE="$DMG_DIR/$APP_NAME.app"
DIST_DIR="$PROJECT_ROOT/dist"

DEBUG=false
CLEAN=false
SOURCE_BINARY=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --debug|-d)
            DEBUG=true
            BUILD_DIR="$PROJECT_ROOT/target/debug"
            shift
            ;;
        --clean)
            CLEAN=true
            shift
            ;;
        --binary)
            if [ $# -lt 2 ]; then
                echo "ERROR: --binary requires a path"
                exit 1
            fi
            SOURCE_BINARY="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [--debug] [--clean] [--binary <path>]"
            echo ""
            echo "Builds NPlank.app and an unsigned DMG in dist/."
            echo "Signing: set DEVELOPER_ID to sign payloads; notarize the DMG yourself."
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_warning() { echo -e "${YELLOW}[WARNING]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1"; }

# Prefer diskutil (hdiutil create/attach are deprecated); fall back to
# hdiutil on older macOS releases.
diskutil_bin() {
    if command -v diskutil >/dev/null 2>&1; then
        command -v diskutil
    elif [ -x /usr/sbin/diskutil ]; then
        echo /usr/sbin/diskutil
    fi
}

create_dmg_from_folder() {
    local src_dir="$1" volname="$2" output="$3"
    local du
    du=$(diskutil_bin)
    if [ -n "$du" ]; then
        "$du" image create from --format UDZO --volumeName "$volname" "$src_dir" "$output"
    else
        hdiutil create -volname "$volname" -srcfolder "$src_dir" -ov -format UDZO "$output"
    fi
}

if ! command -v cargo &> /dev/null; then
    log_error "Rust/Cargo is not installed"
    exit 1
fi

# Build AppIcon.icns from the Plank logo (adapted from SotF create_icns).
create_app_icon() {
    if [ ! -f "$LOGO_PNG" ]; then
        log_warning "Logo not found at $LOGO_PNG, app will use the default icon"
        return 0
    fi
    log_info "Creating app icon from the Plank logo..."
    local iconset_dir="$DMG_DIR/AppIcon.iconset"
    rm -rf "$iconset_dir"
    mkdir -p "$iconset_dir"
    local size
    for size in 16 32 64 128 256 512; do
        sips -s format png -z $size $size "$LOGO_PNG" \
            --out "$iconset_dir/icon_${size}x${size}.png" > /dev/null
        sips -s format png -z $((size * 2)) $((size * 2)) "$LOGO_PNG" \
            --out "$iconset_dir/icon_${size}x${size}@2x.png" > /dev/null
    done
    if iconutil -c icns "$iconset_dir" \
        -o "$APP_BUNDLE/Contents/Resources/AppIcon.icns" 2>/dev/null; then
        log_success "App icon created"
    else
        log_warning "Failed to create icns, app will use the default icon"
    fi
    rm -rf "$iconset_dir"
}

if $CLEAN; then
    log_info "Cleaning staging directory..."
    rm -rf "$DMG_DIR"
fi

if [ -n "$SOURCE_BINARY" ]; then
    log_info "Staging existing binary..."
    case "$SOURCE_BINARY" in
        /*) ;;
        *) SOURCE_BINARY="$PROJECT_ROOT/$SOURCE_BINARY" ;;
    esac
    [ -f "$SOURCE_BINARY" ] || { log_error "Binary not found at $SOURCE_BINARY"; exit 1; }
    mkdir -p "$BUILD_DIR"
    cp "$SOURCE_BINARY" "$BUILD_DIR/$BINARY_NAME"
    chmod +x "$BUILD_DIR/$BINARY_NAME"
else
    log_info "Building nplank + nplankd..."
    cd "$PROJECT_ROOT"
    if $DEBUG; then
        cargo build --bin "$BINARY_NAME" --bin "$DAEMON_BINARY"
    else
        cargo build --release --bin "$BINARY_NAME" --bin "$DAEMON_BINARY"
    fi
fi

for binary in "$BINARY_NAME" "$DAEMON_BINARY"; do
    [ -f "$BUILD_DIR/$binary" ] || { log_error "Binary not found at $BUILD_DIR/$binary"; exit 1; }
done
log_success "Binaries built successfully"

log_info "Creating app bundle..."
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "$BUILD_DIR/$BINARY_NAME" "$BUILD_DIR/$DAEMON_BINARY" "$APP_BUNDLE/Contents/MacOS/"
chmod +x "$APP_BUNDLE/Contents/MacOS/$BINARY_NAME" "$APP_BUNDLE/Contents/MacOS/$DAEMON_BINARY"

if [ -f "$PROJECT_ROOT/THIRD-PARTY-NOTICES.md" ]; then
    cp "$PROJECT_ROOT/THIRD-PARTY-NOTICES.md" "$APP_BUNDLE/Contents/Resources/"
fi

cat > "$APP_BUNDLE/Contents/Info.plist" << EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>${BINARY_NAME}</string>
    <key>CFBundleIdentifier</key>
    <string>${BUNDLE_ID}</string>
    <key>CFBundleName</key>
    <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>
    <string>NPlank</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundleIconName</key>
    <string>AppIcon</string>
    <key>LSMinimumSystemVersion</key>
    <string>15.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF

echo -n "APPL????" > "$APP_BUNDLE/Contents/PkgInfo"

create_app_icon
log_success "App bundle created at $APP_BUNDLE"

ENTITLEMENTS="$PROJECT_ROOT/builds/macos/entitlements.plist"
if [ -n "${DEVELOPER_ID:-}" ]; then
    log_info "Signing payload code with $DEVELOPER_ID..."
    sign_args=(--force --sign "$DEVELOPER_ID" --options runtime --timestamp)
    if [ -f "$ENTITLEMENTS" ]; then
        sign_args+=(--entitlements "$ENTITLEMENTS")
    fi
    codesign "${sign_args[@]}" \
        "$APP_BUNDLE/Contents/MacOS/$BINARY_NAME" \
        "$APP_BUNDLE/Contents/MacOS/$DAEMON_BINARY"
    codesign "${sign_args[@]}" "$APP_BUNDLE"
    codesign --verify --verbose=2 --strict "$APP_BUNDLE"
    if [ -f "$ENTITLEMENTS" ]; then
        codesign -d --entitlements - "$APP_BUNDLE" | grep -q "user-selected.read-write" \
            && log_success "Payload code signed with entitlements" \
            || { log_error "Entitlements missing from signature"; exit 1; }
    else
        log_success "Payload code signed"
    fi
else
    log_warning "DEVELOPER_ID not set: payload left unsigned (expected on a build machine)"
fi

ARCH=$(uname -m)
DMG_FILENAME="$APP_NAME-$VERSION-macos-$ARCH.dmg"
DMG_PATH="$DMG_DIR/$DMG_FILENAME"
rm -f "$DMG_PATH"

log_info "Creating DMG..."
if command -v create-dmg &> /dev/null; then
    log_info "Using create-dmg for styled DMG..."
    if ! create-dmg \
        --volname "$APP_NAME" \
        --window-pos 200 120 \
        --window-size 600 400 \
        --icon-size 100 \
        --icon "$APP_NAME.app" 150 190 \
        --hide-extension "$APP_NAME.app" \
        --app-drop-link 450 185 \
        --no-internet-enable \
        "$DMG_PATH" \
        "$APP_BUNDLE" 2>&1; then
        rm -f "$DMG_DIR"/rw.*.dmg 2>/dev/null || true
        [ -f "$DMG_PATH" ] || { log_error "create-dmg failed and produced no DMG"; exit 1; }
    fi
else
    STAGING_DIR="$DMG_DIR/staging"
    rm -rf "$STAGING_DIR"
    mkdir -p "$STAGING_DIR"
    cp -R "$APP_BUNDLE" "$STAGING_DIR/"
    ln -s /Applications "$STAGING_DIR/Applications"
    create_dmg_from_folder "$STAGING_DIR" "$APP_NAME" "$DMG_PATH"
    rm -rf "$STAGING_DIR"
fi

if [ -n "${DEVELOPER_ID:-}" ]; then
    log_info "Signing DMG container..."
    codesign --force --sign "$DEVELOPER_ID" --timestamp "$DMG_PATH"
    codesign --verify --verbose=2 "$DMG_PATH"
fi

mkdir -p "$DIST_DIR"
cp "$DMG_PATH" "$DIST_DIR/"
log_success "DMG created: $DIST_DIR/$DMG_FILENAME ($(du -h "$DMG_PATH" | cut -f1))"
if [ -z "${DEVELOPER_ID:-}" ]; then
    log_warning "DMG is unsigned: sign and notarize it before distributing (see script header)"
else
    log_warning "DMG is signed but not notarized: submit it with notarytool before distributing"
fi
