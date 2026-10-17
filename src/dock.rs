//! macOS Dock tile badge mirroring sessions that need attention.
//!
//! Called from the UI poll loop; AppKit is only touched when the count
//! actually changes, and only on the main thread.

// Rust guideline compliant 2026-02-21
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSDockTile};
use objc2_foundation::NSString;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Last count pushed to the Dock (`usize::MAX` = never pushed).
static LAST_COUNT: AtomicUsize = AtomicUsize::new(usize::MAX);

/// Sets the Dock badge to `count` awaiting-attention sessions, or clears it
/// at zero. No-ops off the main thread and when the count is unchanged.
pub fn set_attention_count(count: usize) {
    if LAST_COUNT.swap(count, Ordering::Relaxed) == count {
        return;
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let tile: objc2::rc::Retained<NSDockTile> = NSApplication::sharedApplication(mtm).dockTile();
    if count == 0 {
        tile.setBadgeLabel(None);
    } else {
        tile.setBadgeLabel(Some(&NSString::from_str(&count.to_string())));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn badge_count_change_is_tracked() {
        // The counter starts untouched; the first call records, repeats dedupe.
        assert_eq!(super::LAST_COUNT.load(super::Ordering::Relaxed), usize::MAX);
        super::set_attention_count(3);
        assert_eq!(super::LAST_COUNT.load(super::Ordering::Relaxed), 3);
        super::set_attention_count(3);
        assert_eq!(super::LAST_COUNT.load(super::Ordering::Relaxed), 3);
        super::set_attention_count(0);
        assert_eq!(super::LAST_COUNT.load(super::Ordering::Relaxed), 0);
        // Restore so other tests in this process see a pristine state.
        super::LAST_COUNT.store(usize::MAX, super::Ordering::Relaxed);
    }
}
