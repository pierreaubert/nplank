#!/bin/bash
#
# Build the NPlank iOS app with Xcode (adapted from the SotF app-ios flow:
# xcodegen project + build-rust.sh pre-build phase + xcodebuild archive).
#
# Usage:
#   ./scripts/build-ios-app.sh                    # Debug build for simulator
#   ./scripts/build-ios-app.sh --device            # Device build (needs signing)
#   ./scripts/build-ios-app.sh --archive           # App Store archive (needs signing)
#   ./scripts/build-ios-app.sh --clean             # Clean derived data first
#
# Signing:
#   Simulator builds run unsigned (CODE_SIGNING_ALLOWED=NO). Device and
#   archive builds use Xcode automatic signing: set NPLANK_TEAM_ID and make
#   sure the org.spinorama.nplank.ios App ID and provisioning profile exist in
#   your Apple Developer account. This script never creates certificates.
#
# Prerequisites:
#   - Xcode 16+ with iOS 17 SDK, xcodegen (brew install xcodegen)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_DIR="$PROJECT_ROOT/builds/ios"
DERIVED="$PROJECT_ROOT/target/ios-xcode"

MODE="sim"
CLEAN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --device) MODE="device"; shift ;;
        --archive) MODE="archive"; shift ;;
        --clean) CLEAN=true; shift ;;
        --help|-h)
            echo "Usage: $0 [--device] [--archive] [--clean]"
            exit 0
            ;;
        *) echo "Unknown option: $1"; exit 1 ;;
    esac
done

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'
log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1"; }

command -v xcodegen >/dev/null || { log_error "xcodegen not found (brew install xcodegen)"; exit 1; }
command -v xcodebuild >/dev/null || { log_error "xcodebuild not found (Xcode required)"; exit 1; }

if $CLEAN; then
    rm -rf "$DERIVED" "$IOS_DIR/NPlank.xcodeproj"
fi

log_info "Generating Xcode project..."
(cd "$IOS_DIR" && xcodegen generate)

case "$MODE" in
    sim)
        log_info "Building for simulator (unsigned)..."
        xcodebuild build \
            -project "$IOS_DIR/NPlank.xcodeproj" \
            -scheme NPlank \
            -configuration Debug \
            -sdk iphonesimulator \
            -derivedDataPath "$DERIVED" \
            CODE_SIGNING_ALLOWED=NO
        ;;
    device)
        log_info "Building for device..."
        xcodebuild build \
            -project "$IOS_DIR/NPlank.xcodeproj" \
            -scheme NPlank \
            -configuration Release \
            -sdk iphoneos \
            -derivedDataPath "$DERIVED" \
            ${NPLANK_TEAM_ID:+DEVELOPMENT_TEAM="$NPLANK_TEAM_ID"}
        ;;
    archive)
        log_info "Archiving for App Store..."
        rm -rf "$DERIVED/archive"
        xcodebuild archive \
            -project "$IOS_DIR/NPlank.xcodeproj" \
            -scheme NPlank \
            -configuration Release \
            -sdk iphoneos \
            -archivePath "$DERIVED/archive/NPlank.xcarchive" \
            ${NPLANK_TEAM_ID:+DEVELOPMENT_TEAM="$NPLANK_TEAM_ID"}
        log_info "Exporting IPA..."
        xcodebuild -exportArchive \
            -archivePath "$DERIVED/archive/NPlank.xcarchive" \
            -exportPath "$DERIVED/export" \
            -exportOptionsPlist "$IOS_DIR/ExportOptions.plist"
        ;;
esac

log_success "iOS $MODE build complete"
