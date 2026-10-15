#!/bin/bash
#
# Build script for the NPlank iOS static library (adapted from the SotF
# ios.just flow: ios-rust-sim / ios-rust-device + staging for Xcode).
#
# Builds the nplank library for the iOS simulator and for iOS devices, then
# stages both archives under target/ios/ for an Xcode project to link.
#
# Usage:
#   ./scripts/build-ios.sh           # Simulator + device archives
#   ./scripts/build-ios.sh --sim     # Simulator archive only
#   ./scripts/build-ios.sh --device  # Device archive only
#   ./scripts/build-ios.sh --clean   # Clean the staging dir before building
#
# Signing:
#   A static library is not signed. Turning these archives into a shippable
#   app happens in Xcode: add the archive to an app target, set your Team and
#   provisioning profile, then Product > Archive in Xcode signs the bundle
#   with your Developer ID / distribution certificate. That step needs your
#   Apple Developer membership and cannot run on a bare build machine.
#
# Prerequisites:
#   - Xcode Command Line Tools
#   - Rust toolchain from rust-toolchain.toml (iOS targets are added automatically)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

SIM_TARGET="aarch64-apple-ios-sim"
DEVICE_TARGET="aarch64-apple-ios"
STAGE_DIR="$PROJECT_ROOT/target/ios"

BUILD_SIM=true
BUILD_DEVICE=true
CLEAN=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --sim)
            BUILD_DEVICE=false
            shift
            ;;
        --device)
            BUILD_SIM=false
            shift
            ;;
        --clean)
            CLEAN=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--sim] [--device] [--clean]"
            echo ""
            echo "Builds the nplank static library for iOS and stages it under target/ios/."
            echo "Packaging and signing happen in Xcode (see script header)."
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

if ! command -v cargo &> /dev/null; then
    log_error "Rust/Cargo is not installed"
    exit 1
fi

if $CLEAN; then
    log_info "Cleaning staging directory..."
    rm -rf "$STAGE_DIR"
fi
mkdir -p "$STAGE_DIR"

cd "$PROJECT_ROOT"

# The Swift entry point lives in crates/ios-entry: the main crate forbids
# unsafe code and exporting a C symbol to Swift needs one unsafe attribute.
# Xcode links libnplank_ios.a, which pulls in the main crate statically.
build_archive() {
    local target="$1"
    local label="$2"
    log_info "Ensuring Rust target $target..."
    rustup target add "$target"
    log_info "Building nplank iOS entry for $label ($target)..."
    cargo rustc --manifest-path crates/ios-entry/Cargo.toml \
        --release --target "$target" -- --crate-type staticlib
    # crates/ios-entry is its own workspace root: artifacts land under
    # crates/ios-entry/target rather than the main target dir.
    local archive
    archive=$(ls -t "crates/ios-entry/target/$target/release/deps/libnplank_ios_entry.a" 2>/dev/null | head -1)
    [ -n "$archive" ] || { log_error "Archive not found under crates/ios-entry/target/$target/release/deps/"; exit 1; }
    cp "$archive" "$STAGE_DIR/libnplank_ios-$label.a"
    log_success "Staged $STAGE_DIR/libnplank_ios-$label.a ($(du -h "$STAGE_DIR/libnplank_ios-$label.a" | cut -f1))"
}

if $BUILD_SIM; then
    build_archive "$SIM_TARGET" "sim"
fi
if $BUILD_DEVICE; then
    build_archive "$DEVICE_TARGET" "device"
fi

log_success "iOS archives ready under $STAGE_DIR"
log_warning "Unpackaged: link the archive into an Xcode app target, which signs the bundle"
