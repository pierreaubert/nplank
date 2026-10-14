//! Toolkit Input ejected for document navigation and bounded editor caches.
//!
//! Generated using `gpui-toolkit eject Input`; changes stay local to nplank.

// Rust guideline compliant 2026-02-21
#[expect(
    dead_code,
    reason = "Retain the ejected toolkit API for upstream comparisons"
)]
mod input;
pub(crate) use input::{Input, InputSize, cleanup_stale_input_states};
