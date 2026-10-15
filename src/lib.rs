//! Durable workspaces, isolated Plank execution, and a shared native client protocol.

// Rust guideline compliant 2026-02-21
#[cfg(not(target_family = "wasm"))]
mod agents;
#[cfg(not(target_family = "wasm"))]
mod artifacts;
#[cfg(all(feature = "gui", target_family = "wasm"))]
mod browser_preferences;
#[cfg(feature = "gui")]
mod buddy_view;
#[path = "../plank/src/buddyframe.rs"]
pub mod buddyframe;
pub mod client;
pub mod commands;
#[cfg(not(target_family = "wasm"))]
pub mod daemon;
#[path = "../plank/src/documenttransfer.rs"]
pub mod documenttransfer;
#[cfg(not(target_family = "wasm"))]
mod focus;
#[path = "../plank/src/guiframe.rs"]
pub mod frame;
#[path = "../plank/src/guifocus.rs"]
pub mod guifocus;
#[path = "../plank/src/insightchart.rs"]
pub mod insightchart;
#[path = "../plank/src/joboutput.rs"]
pub mod joboutput;
#[path = "../plank/src/livecontrol.rs"]
pub mod livecontrol;
#[cfg(not(target_family = "wasm"))]
mod metadata;
pub mod model;
pub mod notifications;
#[cfg(any(test, all(feature = "gui", target_family = "wasm")))]
mod preference_queue;
#[cfg(any(feature = "gui", test))]
mod preview;
#[cfg(not(target_family = "wasm"))]
pub mod store;
#[path = "../plank/src/subhistory.rs"]
pub mod subhistory;
#[cfg(not(target_family = "wasm"))]
mod web_host;
#[cfg(not(target_family = "wasm"))]
mod workers;

#[cfg(feature = "gui")]
mod document;
#[cfg(feature = "gui")]
mod editor_input;
#[cfg(feature = "gui")]
mod frame_view;
#[cfg(feature = "gui")]
mod report;
#[cfg(any(feature = "gui", test))]
mod transcript;
#[cfg(feature = "gui")]
pub mod ui;

#[cfg(all(feature = "gui", target_os = "macos"))]
mod utilization;

#[cfg(all(feature = "gui", target_os = "macos"))]
pub mod native;

#[cfg(all(feature = "gui", target_family = "wasm"))]
mod web;

// ComponentTheme currently expands through the host crate's theme namespace.
#[cfg(feature = "gui")]
use gpui_ui_kit::theme;
