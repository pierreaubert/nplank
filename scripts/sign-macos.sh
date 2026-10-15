#!/bin/bash
#
# Sign and notarize macOS artifacts in dist/ (adapted from the SotF
# scripts/sign-macos.sh flow).
#
# Signs DMGs (the NPlank.app bundle inside, then the DMG container) and bare
# Mach-O binaries produced by scripts/build-macos.sh. Run this AFTER
# build-macos.sh when DEVELOPER_ID was not set during the build, or to add
# notarization to an already-signed DMG.
#
# Usage:
#   ./scripts/sign-macos.sh                    # Sign all artifacts in dist/
#   ./scripts/sign-macos.sh --notarize         # Sign and notarize
#   ./scripts/sign-macos.sh dist/NPlank-0.1.0-macos-arm64.dmg  # One file
#
# Environment variables:
#   DEVELOPER_ID             - Developer ID Application certificate name
#                              Example: "Developer ID Application: Your Name (TEAMID)"
#   APPLE_ID                 - Apple ID email for notarization
#   NOTARY_PROFILE           - Keychain profile for notarytool (default: nplank-notarization)
#   NPLANK_ENTITLEMENTS      - Entitlements plist applied to NPlank.app
#                              (default: builds/macos/entitlements.plist)
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST_DIR="$PROJECT_ROOT/dist"
NOTARY_PROFILE="${NOTARY_PROFILE:-nplank-notarization}"
DEFAULT_ENTITLEMENTS="$PROJECT_ROOT/builds/macos/entitlements.plist"
NPLANK_ENTITLEMENTS="${NPLANK_ENTITLEMENTS:-$DEFAULT_ENTITLEMENTS}"

VERSION=$(grep -m1 '^version = ' "$PROJECT_ROOT/Cargo.toml" | sed -E 's/version = "(.*)"/\1/')
if [ -z "$VERSION" ]; then
    echo "ERROR: Could not extract version from Cargo.toml"
    exit 1
fi

NOTARIZE=false
SPECIFIC_FILES=()

while [[ $# -gt 0 ]]; do
    case $1 in
        --notarize)
            NOTARIZE=true
            shift
            ;;
        --help|-h)
            echo "Usage: $0 [--notarize] [file ...]"
            echo ""
            echo "Signs NPlank macOS artifacts (DMG, bare binaries) in dist/ or specific files."
            echo ""
            echo "Options:"
            echo "  --notarize    Also notarize with Apple (requires APPLE_ID)"
            echo ""
            echo "Environment variables:"
            echo "  DEVELOPER_ID        Developer ID Application certificate"
            echo "  APPLE_ID            Apple ID for notarization"
            echo "  NOTARY_PROFILE      Keychain profile for notarytool (default: $NOTARY_PROFILE)"
            echo "  NPLANK_ENTITLEMENTS Optional entitlements plist for NPlank.app"
            exit 0
            ;;
        *)
            SPECIFIC_FILES+=("$1")
            shift
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
# hdiutil on older macOS releases. Detach stays on hdiutil (not deprecated).
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

attach_dmg() {
    local dmg_path="$1" mount_point="$2"
    local du
    du=$(diskutil_bin)
    if [ -n "$du" ]; then
        "$du" image attach --nobrowse --mountPoint "$mount_point" "$dmg_path" >/dev/null
    else
        hdiutil attach "$dmg_path" -mountpoint "$mount_point" -nobrowse -quiet
    fi
}

check_prerequisites() {
    if [ -z "${DEVELOPER_ID:-}" ]; then
        log_error "DEVELOPER_ID environment variable not set"
        log_info "Set it to your Developer ID certificate name, e.g.:"
        log_info "  export DEVELOPER_ID='Developer ID Application: Your Name (TEAMID)'"
        exit 1
    fi

    if $NOTARIZE && [ -z "${APPLE_ID:-}" ]; then
        log_error "APPLE_ID environment variable not set for notarization"
        exit 1
    fi

    if ! command -v codesign &> /dev/null; then
        log_error "codesign not found (Xcode Command Line Tools required)"
        exit 1
    fi
}

is_macho_file() {
    local file_path="$1"
    [ -f "$file_path" ] && file "$file_path" | grep -q "Mach-O"
}

is_nplank_code() {
    local code_path="$1"
    case "${code_path##*/}" in
        NPlank.app|nplank|nplankd|NPlank-*-macos-*) return 0 ;;
    esac
    [ -f "$code_path/Contents/MacOS/nplank" ]
}

sign_macho_file() {
    codesign --force --sign "$DEVELOPER_ID" --options runtime --timestamp "$1"
}

sign_code_bundle() {
    local bundle_path="$1"
    local -a sign_args=(--force --sign "$DEVELOPER_ID" --options runtime --timestamp)
    if [ -f "${NPLANK_ENTITLEMENTS:-}" ] && is_nplank_code "$bundle_path"; then
        sign_args+=(--entitlements "$NPLANK_ENTITLEMENTS")
    elif ! is_nplank_code "$bundle_path"; then
        # Non-NPlank nested code: sign recursively.
        sign_args+=(--deep)
    fi
    # NPlank bundles without an entitlements file keep their payload
    # signatures: sign without --deep.

    codesign "${sign_args[@]}" "$bundle_path"
    codesign --verify --verbose=2 --strict "$bundle_path"
}

sign_payload_code() {
    local root_dir="$1"

    log_info "  Signing nested Mach-O payloads..."
    while IFS= read -r -d '' candidate; do
        if is_macho_file "$candidate"; then
            sign_macho_file "$candidate"
            log_info "    Signed Mach-O: ${candidate#$root_dir/}"
        fi
    done < <(find "$root_dir" -type f -print0)

    log_info "  Signing code bundles..."
    while IFS= read -r bundle; do
        [ -d "$bundle" ] || continue
        sign_code_bundle "$bundle"
        log_info "    Signed bundle: ${bundle#$root_dir/}"
    done < <(
        find "$root_dir" -type d \( \
            -name "*.app" -o \
            -name "*.appex" -o \
            -name "*.bundle" -o \
            -name "*.framework" -o \
            -name "*.xpc" \
        \) -print | awk '{ print length($0) "\t" $0 }' | sort -rn | cut -f2-
    )
}

# Sign a DMG file (signs the app bundle inside, then the DMG container).
sign_dmg() {
    local dmg_path="$1"
    local dmg_name
    dmg_name=$(basename "$dmg_path")
    log_info "Signing DMG: $dmg_name"

    local mount_point
    mount_point=$(mktemp -d)
    attach_dmg "$dmg_path" "$mount_point"

    local app_bundle=""
    for app in "$mount_point"/*.app; do
        if [ -d "$app" ]; then
            app_bundle="$app"
            break
        fi
    done

    if [ -z "$app_bundle" ]; then
        hdiutil detach "$mount_point" -quiet
        rmdir "$mount_point"
        log_error "No .app bundle found inside DMG"
        exit 1
    fi

    local app_name
    app_name=$(basename "$app_bundle")
    log_info "  Found app bundle: $app_name"

    # The DMG is read-only: stage a writable copy, sign it, rebuild the DMG.
    local staging_dir
    staging_dir=$(mktemp -d)
    cp -R "$app_bundle" "$staging_dir/"

    for item in "$mount_point"/*; do
        local item_name
        item_name=$(basename "$item")
        if [ "$item_name" != "$app_name" ]; then
            cp -R "$item" "$staging_dir/" 2>/dev/null || true
        fi
    done

    hdiutil detach "$mount_point" -quiet
    rmdir "$mount_point"

    local staged_app="$staging_dir/$app_name"

    sign_payload_code "$staging_dir"

    codesign --verify --verbose=2 --strict "$staged_app"
    log_success "App bundle signed and verified"

    rm -f "$dmg_path"
    create_dmg_from_folder "$staging_dir" "${app_name%.app}" "$dmg_path"

    codesign --force --sign "$DEVELOPER_ID" --timestamp "$dmg_path"

    rm -rf "$staging_dir"

    codesign --verify --verbose=2 "$dmg_path"
    log_success "DMG signed: $dmg_name"
}

notarize_file() {
    local file_path="$1"

    if ! $NOTARIZE; then
        return
    fi

    log_info "Submitting for notarization: $(basename "$file_path")"

    local submission_output
    submission_output=$(xcrun notarytool submit "$file_path" \
        --apple-id "$APPLE_ID" \
        --keychain-profile "$NOTARY_PROFILE" \
        --wait 2>&1)

    echo "$submission_output"

    if echo "$submission_output" | grep -q "status: Accepted"; then
        log_success "Notarization accepted"

        log_info "Stapling notarization ticket..."
        xcrun stapler staple "$file_path"
        xcrun stapler validate "$file_path"
        log_success "Notarization ticket stapled: $(basename "$file_path")"
    else
        log_error "Notarization failed for $(basename "$file_path")"
        local submission_id
        submission_id=$(echo "$submission_output" | grep -o 'id: [a-f0-9-]*' | head -1 | cut -d' ' -f2)
        if [ -n "$submission_id" ]; then
            log_info "To get detailed logs, run:"
            log_info "  xcrun notarytool log $submission_id --apple-id $APPLE_ID --keychain-profile $NOTARY_PROFILE"
        fi
        exit 1
    fi
}

sign_file() {
    local file_path="$1"

    if [ ! -f "$file_path" ]; then
        log_warning "File not found: $file_path"
        return
    fi

    case "$file_path" in
        *.dmg)
            sign_dmg "$file_path"
            notarize_file "$file_path"
            ;;
        *)
            # Bare Mach-O binaries (NPlank-VERSION-macos-*) are signed alongside
            # DMG containers so direct-download artifacts remain valid.
            if is_macho_file "$file_path"; then
                sign_macho_file "$file_path"
                codesign --verify --verbose=2 --strict "$file_path"
                # notarytool can ingest a single Mach-O wrapped in a zip; the
                # ticket is verified at runtime via online check (Mach-O has no
                # place to staple a ticket).
                notarize_file "$file_path"
            else
                log_warning "Unknown file type, skipping: $(basename "$file_path")"
            fi
            ;;
    esac
}

main() {
    log_info "=========================================="
    log_info "NPlank macOS Code Signing v${VERSION}"
    log_info "=========================================="

    check_prerequisites

    if [ ${#SPECIFIC_FILES[@]} -gt 0 ]; then
        for f in "${SPECIFIC_FILES[@]}"; do
            sign_file "$f"
        done
    else
        local found=false
        shopt -s nullglob
        for f in "$DIST_DIR"/NPlank-"$VERSION"*.dmg \
                 "$DIST_DIR"/NPlank-"$VERSION"-macos-*; do
            if [ -f "$f" ]; then
                sign_file "$f"
                found=true
            fi
        done
        shopt -u nullglob

        if ! $found; then
            log_warning "No NPlank artifacts for v${VERSION} found in $DIST_DIR"
            log_info "Run scripts/build-macos.sh first"
            exit 1
        fi
    fi

    log_info "=========================================="
    log_success "Signing complete!"
    log_info "=========================================="
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
