//! Browser startup and authentication for the shared GPUI workspace view.
//!
//! Credentials remain in page memory. Engines and SQLite always run in the daemon.

// Rust guideline compliant 2026-02-21
use std::path::PathBuf;

use gpui::{AppContext, Context, Entity, IntoElement, Render, Window, div, prelude::*};
use gpui_miniapp::{MiniApp, MiniAppConfig};
use gpui_ui_kit::{Button, Input, ThemeExt};
use gpui_ui_kit_macros::layout;
use wasm_bindgen::{JsCast, prelude::*};

use crate::{client::Client, ui::WorkspaceView};

struct BrowserView {
    token: String,
    error: Option<String>,
    connecting: bool,
    workspace: Option<Entity<WorkspaceView>>,
}

impl std::fmt::Debug for BrowserView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserView")
            .field("connecting", &self.connecting)
            .field("has_workspace", &self.workspace.is_some())
            .finish_non_exhaustive()
    }
}

impl BrowserView {
    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.connecting || self.token.trim().is_empty() {
            return;
        }
        let origin = web_sys::window()
            .and_then(|window| window.location().origin().ok())
            .unwrap_or_default();
        let client = match Client::new(&origin, self.token.trim().into()) {
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
        cx.spawn(async move |entity, cx| {
            let result = client.snapshot(None, 0).await;
            let _ = entity.update(cx, |view, cx| {
                view.connecting = false;
                match result {
                    Ok(_) => {
                        view.token.clear();
                        let appearance = gpui_miniapp::web_query_param("appearance").is_some();
                        view.workspace =
                            Some(cx.new(|cx| {
                                WorkspaceView::new(client, PathBuf::new(), appearance, cx)
                            }));
                    }
                    Err(error) => view.error = Some(format!("Could not connect: {error}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for BrowserView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(workspace) = &self.workspace {
            return div()
                .size_full()
                .child(workspace.clone())
                .into_any_element();
        }
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
        .disabled(self.connecting || self.token.trim().is_empty())
        .on_click(move |_, cx| {
            let _ = weak.update(cx, |view, cx| view.connect(cx));
        });
        let theme = cx.theme();
        let mut content = div()
            .size_full()
            .p_6()
            .bg(theme.background)
            .text_color(theme.text_primary)
            .child(layout! { V[g4] {
                Hd "Connect to your workspace" [level="1"],
                Tx "Your agents run in the daemon. Closing this page leaves runs alive." [muted],
                Tx.sm "Paste the token from the daemon’s data directory. It stays in page memory." [muted]
            }})
            .child(token)
            .child(connect);
        if let Some(error) = &self.error {
            content = content.child(div().p_3().child(error.clone()));
        }
        content.into_any_element()
    }
}

#[wasm_bindgen(start)]
/// Starts the browser client after the generated WASM loader initializes.
pub fn start() {
    gpui_miniapp::web_init();
    MiniApp::run(
        MiniAppConfig::new("nplank")
            .with_theme(true)
            .scrollable(false),
        |cx| {
            cx.new(|_| BrowserView {
                token: String::new(),
                error: None,
                connecting: false,
                workspace: None,
            })
        },
    );
}

pub(crate) async fn sleep(milliseconds: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                resolve.unchecked_ref(),
                milliseconds,
            );
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Starts a prepared browser download from a current user click.
pub(crate) fn save_file(url: &str, name: &str) -> anyhow::Result<()> {
    let document = web_sys::window()
        .and_then(|window| window.document())
        .ok_or_else(|| anyhow::anyhow!("Browser document unavailable"))?;
    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| anyhow::anyhow!("Cannot prepare file download"))?
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("Cannot prepare file download"))?;
    anchor.set_href(url);
    anchor.set_download(name);
    document
        .body()
        .ok_or_else(|| anyhow::anyhow!("Browser page unavailable"))?
        .append_child(&anchor)
        .map_err(|_| anyhow::anyhow!("Cannot prepare file download"))?;
    anchor.click();
    anchor.remove();
    Ok(())
}
