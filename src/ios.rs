//! iOS client entry point for the shared GPUI workspace view.
//!
//! iOS cannot spawn engine subprocesses, so this client always attaches to a
//! daemon over the network, exactly like the browser client. Credentials stay
//! in memory; engines and SQLite always run in the daemon.

// Rust guideline compliant 2026-02-21
use gpui::{AppContext, Context, Entity, IntoElement, Render, Window, div, prelude::*};
use gpui_ui_kit::{Button, Input, ThemeExt};
use gpui_ui_kit_macros::layout;

use crate::{client::Client, ui::WorkspaceView};
use std::borrow::Cow;
use std::path::PathBuf;

#[derive(Debug)]
struct Assets;

impl gpui::AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/cpu.svg" => Some(Cow::Borrowed(include_bytes!("../assets/cpu.svg"))),
            "icons/gpu.svg" => Some(Cow::Borrowed(include_bytes!("../assets/gpu.svg"))),
            _ => None,
        })
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<gpui::SharedString>> {
        Ok(vec![])
    }
}

/// Set by the Scan button; Swift's display-link tick drains it through the
/// shim and presents the camera scanner. Atomic so the UI thread can set it
/// without any FFI call from this crate (which forbids unsafe code).
pub static SCAN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Pending pairing credentials from a scanned QR code, stored by the shim.
static PENDING_PAIR: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);

/// Queues scanned pairing credentials for the connect view to pick up.
pub fn store_pair(url: String, token: String) {
    if let Ok(mut pending) = PENDING_PAIR.lock() {
        *pending = Some((url, token));
    }
}

/// Takes a pending scan request, resetting the flag.
pub fn take_scan_request() -> bool {
    SCAN_REQUESTED.swap(false, std::sync::atomic::Ordering::SeqCst)
}

fn take_pair() -> Option<(String, String)> {
    PENDING_PAIR
        .lock()
        .ok()
        .and_then(|mut pending| pending.take())
}

struct ConnectView {
    docs_dir: PathBuf,
    url: String,
    token: String,
    error: Option<String>,
    connecting: bool,
    auto_connect: bool,
    workspace: Option<Entity<WorkspaceView>>,
}

impl std::fmt::Debug for ConnectView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectView")
            .field("connecting", &self.connecting)
            .field("has_workspace", &self.workspace.is_some())
            .finish_non_exhaustive()
    }
}

impl ConnectView {
    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting || self.url.trim().is_empty() || self.token.trim().is_empty() {
            return;
        }
        let client = match Client::new(self.url.trim(), self.token.trim().into()) {
            Ok(client) => client,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.connecting = true;
        self.error = None;
        cx.notify();
        let docs_dir = self.docs_dir.clone();
        cx.spawn(async move |entity, cx| {
            let result = client.snapshot(None, 0).await;
            let _ = entity.update(cx, |view: &mut ConnectView, cx| {
                view.connecting = false;
                match result {
                    Ok(_) => {
                        view.token.clear();
                        view.workspace =
                            Some(cx.new(|cx| WorkspaceView::new(client, docs_dir, false, cx)));
                    }
                    Err(error) => view.error = Some(format!("Could not connect: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ConnectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = &self.workspace {
            return div()
                .size_full()
                .child(workspace.clone())
                .into_any_element();
        }
        // A scanned QR code fills the fields and connects without further taps.
        if let Some((url, token)) = take_pair() {
            self.url = url;
            self.token = token;
            self.auto_connect = true;
            cx.notify();
        }
        if self.auto_connect
            && !self.connecting
            && !self.url.trim().is_empty()
            && !self.token.trim().is_empty()
        {
            self.auto_connect = false;
            self.connect(cx);
            cx.notify();
        }
        let weak = cx.entity().downgrade();
        let url = Input::new("daemon-url")
            .label("Daemon URL")
            .value(self.url.clone())
            .on_text_change(move |text, _, cx| {
                let _ = weak.update(cx, |view, cx| {
                    view.url = text.into();
                    cx.notify();
                });
            });
        let weak = cx.entity().downgrade();
        let token = Input::new("daemon-token")
            .label("Daemon access token")
            .password(true)
            .value(self.token.clone())
            .on_text_change(move |text, _, cx| {
                let _ = weak.update(cx, |view, cx| {
                    view.token = text.into();
                    cx.notify();
                });
            });
        let weak = cx.entity().downgrade();
        let connect = Button::new(
            "connect",
            if self.connecting {
                "Connecting…"
            } else {
                "Connect"
            },
        )
        .disabled(self.connecting || self.url.trim().is_empty() || self.token.trim().is_empty())
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |view, cx| view.connect(cx));
        });
        let weak = cx.entity().downgrade();
        let scan = Button::new("scan-qr", "Scan QR code").on_click(move |_, cx| {
            SCAN_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = weak.update(cx, |_, cx| cx.notify());
        });
        let theme = cx.theme();
        let mut content = div()
            .size_full()
            .p_6()
            .bg(theme.background)
            .text_color(theme.text_primary)
            .child(layout! { V[g4] {
                Hd "Connect to your workspace" [level="1"],
                Tx "Your agents run in the daemon. Closing this app leaves runs alive." [muted],
                Tx.sm "Enter the daemon address and token. Both stay in app memory." [muted]
            }})
            .child(url)
            .child(token)
            .child(connect)
            .child(scan);
        if let Some(error) = &self.error {
            content = content.child(div().p_3().child(error.clone()));
        }
        content.into_any_element()
    }
}

/// Starts the iOS client against the app's documents directory, which backs
/// preferences and drafts.
pub fn start_ios_client(docs: PathBuf) {
    eprintln!("[iOS] nplank_ios_start, preferences at {}", docs.display());

    gpui_ios::ios::ffi::set_asset_source(Assets);
    gpui_ios::ios::ffi::set_app_callback(Box::new(move |cx: &mut gpui::App| {
        cx.open_window(gpui::WindowOptions::default(), |_, cx| {
            cx.new(|_cx| ConnectView {
                docs_dir: docs.clone(),
                url: String::new(),
                token: String::new(),
                error: None,
                connecting: false,
                auto_connect: false,
                workspace: None,
            })
        })
        .expect("iOS connect window must open");
        cx.activate(true);
    }));
    gpui_ios::ios::ffi::run_app();
}
