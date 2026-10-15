//! C entry point for the iOS host app.
//!
//! Lives in its own crate because the main crate forbids unsafe code and
//! exporting a symbol to Swift requires one `unsafe` attribute plus a raw
//! pointer read. Everything else stays in safe code behind
//! [`nplank::ios::start_ios_client`].

#[cfg(target_os = "ios")]
use std::ffi::CStr;
#[cfg(target_os = "ios")]
use std::os::raw::c_char;

/// Starts the iOS client; called once from Swift with the app's documents
/// directory, which backs preferences and drafts.
///
/// # Safety
/// `docs_dir` must be null or point to a valid NUL-terminated UTF-8 string
/// that stays alive for the duration of this call.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn nplank_ios_start(docs_dir: *const c_char) {
    let docs = if docs_dir.is_null() {
        std::env::temp_dir()
    } else {
        // SAFETY: the Swift caller guarantees a valid NUL-terminated UTF-8
        // string for the duration of this call (checked for null above).
        let raw = unsafe { CStr::from_ptr(docs_dir) };
        raw.to_string_lossy().into_owned().into()
    };
    nplank::ios::start_ios_client(docs);
}

/// Polled by Swift on every display-link tick; presents the camera scanner
/// when the connect view's Scan button was tapped.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn nplank_ios_take_scan_request() -> bool {
    nplank::ios::take_scan_request()
}

/// Called from Swift when the camera scanner reads a pairing code.
///
/// Accepts the shared pairing payload `{"v":1,"url","token"}` produced by the
/// desktop `/pair` dialog; anything else is ignored.
#[cfg(target_os = "ios")]
#[unsafe(no_mangle)]
pub extern "C" fn nplank_ios_qr_scanned(payload: *const c_char) {
    if payload.is_null() {
        return;
    }
    // SAFETY: the Swift caller keeps `payload` valid for the duration of this
    // call. The bytes are copied into an owned string immediately.
    let text = match unsafe { CStr::from_ptr(payload) }.to_str() {
        Ok(text) => text,
        Err(_) => return,
    };
    if let Some((url, token)) = nplank::client::Client::parse_pair_payload(text) {
        nplank::ios::store_pair(url, token);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    fn ios_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../builds/ios")
    }

    fn read(name: &str) -> String {
        std::fs::read_to_string(ios_dir().join(name)).expect("iOS scaffold file must exist")
    }

    #[test]
    fn swift_and_rust_agree_on_pairing_symbols() {
        let bridging = read("NPlank/BridgingHeader.h");
        for symbol in [
            "nplank_ios_start",
            "nplank_ios_take_scan_request",
            "nplank_ios_qr_scanned",
        ] {
            assert!(
                bridging.contains(symbol),
                "BridgingHeader.h must declare {symbol}"
            );
        }
        let scanner = read("NPlank/QRScanner.swift");
        assert!(scanner.contains("nplank_ios_qr_scanned"));
        let delegate = read("NPlank/AppDelegate.swift");
        assert!(delegate.contains("nplank_ios_take_scan_request"));
        let plist = read("NPlank/Info.plist");
        assert!(plist.contains("NSCameraUsageDescription"));
    }
}
