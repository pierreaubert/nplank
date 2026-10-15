#!/bin/bash
#
# Upload the NPlank iOS app to TestFlight via App Store Connect.
#
# iOS apps are not notarized; Apple signs and reviews them through App Store
# Connect instead. This script archives, then exports with destination
# `upload`, which submits the build to TestFlight in one step.
#
# Usage:
#   ./scripts/upload-testflight.sh
#
# Required environment:
#   NPLANK_TEAM_ID        Apple Developer Team ID (also used for signing)
#   ASC_KEY_ID            App Store Connect API key ID
#   ASC_ISSUER_ID         App Store Connect issuer ID
#   ASC_KEY_PATH          Path to the .p8 API key (default: builds/ios/AuthKey.p8)
#
# The bundle ID is org.spinorama.nplank.ios; register it in App Store Connect
# (with a TestFlight beta group) before the first upload. Place the .p8 key
# next to this script or point ASC_KEY_PATH at it; never commit the key.
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
IOS_DIR="$PROJECT_ROOT/builds/ios"
DERIVED="$PROJECT_ROOT/target/ios-xcode"

: "${NPLANK_TEAM_ID:?Set NPLANK_TEAM_ID to your Apple Developer Team ID}"
: "${ASC_KEY_ID:?Set ASC_KEY_ID to your App Store Connect API key ID}"
: "${ASC_ISSUER_ID:?Set ASC_ISSUER_ID to your App Store Connect issuer ID}"
ASC_KEY_PATH="${ASC_KEY_PATH:-$IOS_DIR/AuthKey.p8}"
[ -f "$ASC_KEY_PATH" ] || {
    echo "API key not found at $ASC_KEY_PATH (set ASC_KEY_PATH)"
    exit 1
}

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'
log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_success() { echo -e "${GREEN}[SUCCESS]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1"; }

log_info "Archiving (signs the bundle with your distribution identity)..."
NPLANK_TEAM_ID="$NPLANK_TEAM_ID" "$SCRIPT_DIR/build-ios-app.sh" --archive

log_info "Exporting with destination=upload (submits to TestFlight)..."
xcodebuild -exportArchive \
    -archivePath "$DERIVED/archive/NPlank.xcarchive" \
    -exportOptionsPlist "$IOS_DIR/ExportOptions-upload.plist" \
    -authenticationKeyPath "$ASC_KEY_PATH" \
    -authenticationKeyID "$ASC_KEY_ID" \
    -authenticationKeyIssuerID "$ASC_ISSUER_ID" \
    -allowProvisioningUpdates

log_success "Upload submitted; the build appears in TestFlight after Apple processing"
