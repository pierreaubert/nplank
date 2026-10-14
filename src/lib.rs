//! Durable workspaces, isolated Plank execution, and a shared native client protocol.

// Rust guideline compliant 2026-02-21
#[cfg(not(target_family = "wasm"))]
mod agents;
#[cfg(not(target_family = "wasm"))]
mod artifacts;
pub mod client;
pub mod commands;
#[cfg(not(target_family = "wasm"))]
pub mod daemon;
#[cfg(not(target_family = "wasm"))]
mod metadata;
pub mod model;
#[cfg(not(target_family = "wasm"))]
pub mod store;
#[cfg(not(target_family = "wasm"))]
mod web_host;
#[cfg(not(target_family = "wasm"))]
mod workers;

#[cfg(feature = "gui")]
mod document;
#[cfg(feature = "gui")]
mod editor_input;
#[cfg(feature = "gui")]
mod report;
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
