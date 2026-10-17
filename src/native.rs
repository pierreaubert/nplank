//! macOS GPUI host with an overlay titlebar and native window controls.

// Rust guideline compliant 2026-02-21
use crate::ui::{
    NewAgent, NewWorkspace, NextAttention, OpenCommands, OpenPreferences, PairDevice, PasteImage,
    SelectAppearance, ToggleArchived, ToggleCi, ToggleDocs, ToggleFleet, ToggleMonitor,
    WorkspaceView,
};
use gpui::*;
use gpui_design::{DesignSystem, DesignSystemState};
use gpui_miniapp::{
    MiniAppConfig, MiniAppState, current_platform, load_miniapp_state, save_miniapp_state,
};
use gpui_ui_kit::{accessibility::AccessibilityTree, theme::ThemeState};
use std::borrow::Cow;

#[derive(Debug)]
struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/cpu.svg" => Some(Cow::Borrowed(include_bytes!("../assets/cpu.svg"))),
            "icons/gpu.svg" => Some(Cow::Borrowed(include_bytes!("../assets/gpu.svg"))),
            "icons/chatgpt.svg" => Some(Cow::Borrowed(include_bytes!("../assets/chatgpt.svg"))),
            "icons/muse.svg" => Some(Cow::Borrowed(include_bytes!("../assets/muse.svg"))),
            _ => None,
        })
    }
    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

actions!(
    nplank,
    [
        Quit,
        CloseWindow,
        Minimize,
        Zoom,
        Cut,
        Copy,
        Paste,
        SelectAll,
        Undo,
        Redo
    ]
);

/// Runs a toolkit view beneath native macOS traffic lights.
///
/// # Errors
/// Returns platform initialization errors. Window creation failures are printed.
pub fn run(
    mut config: MiniAppConfig,
    build: impl FnOnce(&mut App) -> Entity<WorkspaceView> + 'static,
) -> Result<()> {
    if let Some(state) = config.state_file.as_deref().and_then(load_miniapp_state) {
        config.width = state.width.unwrap_or(config.width);
        config.height = state.height.unwrap_or(config.height);
    }
    let platform = current_platform().map_err(anyhow::Error::msg)?;
    Application::with_platform(platform)
        .with_assets(Assets)
        .run(move |cx| {
            cx.set_global(ThemeState::with_variant(config.initial_theme));
            cx.set_global(DesignSystemState::with_system(
                DesignSystem::platform_default(),
            ));
            cx.set_global(AccessibilityTree::new());
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            let display = cx
                .primary_display()
                .map(|display| display.visible_bounds().size);
            let initial_size = size(px(config.width.max(360.0)), px(config.height.max(300.0)));
            let initial_size = display.map_or(initial_size, |display| initial_size.min(&display));
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        initial_size,
                        cx,
                    ))),
                    window_min_size: Some(size(px(360.0), px(300.0))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(config.title.clone()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    ..Default::default()
                },
                move |_, cx| build(cx),
            );
            let Ok(window) = window else {
                eprintln!("Could not open nplank window");
                cx.quit();
                return;
            };
            cx.on_action(move |_: &NewAgent, cx| {
                let _ = window.update(cx, |view, _, cx| view.add_agent(None, cx));
            });
            cx.on_action(move |_: &NewWorkspace, cx| {
                let _ = window.update(cx, |view, _, cx| view.add_workspace(cx));
            });
            cx.on_action(move |_: &OpenPreferences, cx| {
                let _ = window.update(cx, |view, _, cx| view.open_preferences(cx));
            });
            cx.on_action(move |_: &ToggleArchived, cx| {
                let _ = window.update(cx, |view, _, cx| view.toggle_archived(cx));
            });
            cx.on_action(move |_: &ToggleFleet, cx| {
                let _ = window.update(cx, |view, _, cx| view.toggle_fleet(cx));
            });
            cx.on_action(move |_: &ToggleCi, cx| {
                let _ = window.update(cx, |view, _, cx| view.toggle_ci(cx));
            });
            cx.on_action(move |_: &ToggleDocs, cx| {
                let _ = window.update(cx, |view, _, cx| view.toggle_docs(cx));
            });
            cx.on_action(move |_: &ToggleMonitor, cx| {
                let _ = window.update(cx, |view, _, cx| view.toggle_monitor(cx));
            });
            cx.on_action(move |_: &NextAttention, cx| {
                let _ = window.update(cx, |view, _, cx| view.go_next_attention(cx));
            });
            cx.on_action(move |_: &PasteImage, cx| {
                let _ = window.update(cx, |view, _, cx| {
                    if view.paste_clipboard_image(cx) {
                        cx.notify();
                    }
                });
            });
            cx.on_action(move |_: &PairDevice, cx| {
                let _ = window.update(cx, |view, _, cx| view.pair_device(cx));
            });
            cx.on_action(move |_: &OpenCommands, cx| {
                let _ = window.update(cx, |view, _, cx| view.open_commands(cx));
            });
            cx.on_action(move |selection: &SelectAppearance, cx| {
                let _ = window.update(cx, |view, _, cx| view.select_appearance(selection, cx));
            });
            cx.on_action(move |_: &CloseWindow, cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            });
            cx.on_action(move |_: &Minimize, cx| {
                let _ = window.update(cx, |_, window, _| window.minimize_window());
            });
            cx.on_action(move |_: &Zoom, cx| {
                let _ = window.update(cx, |_, window, _| window.zoom_window());
            });
            macro_rules! edit_handler {
                ($action:ty, $key:literal) => {
                    cx.on_action(move |_: &$action, cx| {
                        let _ = window.update(cx, |_, window, cx| {
                            if let Ok(key) = Keystroke::parse($key) {
                                window.dispatch_keystroke(key, cx);
                            }
                        });
                    });
                };
            }
            edit_handler!(Cut, "cmd-x");
            edit_handler!(Copy, "cmd-c");
            edit_handler!(Paste, "cmd-v");
            edit_handler!(SelectAll, "cmd-a");
            edit_handler!(Undo, "cmd-z");
            edit_handler!(Redo, "cmd-shift-z");
            cx.bind_keys([
                KeyBinding::new("cmd-w", CloseWindow, None),
                KeyBinding::new("cmd-m", Minimize, None),
            ]);
            cx.set_menus(menus());
            let state_file = config.state_file;
            // Capture geometry while the window still exists, including Command-Q.
            let subscription = cx.on_app_quit(move |cx| {
                if let Some(path) = &state_file {
                    let geometry = window
                        .update(cx, |_, window, _| window.viewport_size())
                        .ok();
                    let geometry = geometry.unwrap_or(initial_size);
                    let state = MiniAppState::snapshot(
                        f32::from(geometry.width),
                        f32::from(geometry.height),
                        cx.global::<ThemeState>().theme.variant,
                        false,
                        config.initial_language,
                    );
                    if let Err(error) = save_miniapp_state(path, &state) {
                        eprintln!("Could not save window state: {error}");
                    }
                }
                async {}
            });
            subscription.detach();
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
            nplank_macos_shim::install_status_item();
            if nplank_macos_shim::inside_app_bundle() {
                nplank_macos_shim::install_notification_delegate();
            }
        });
    Ok(())
}

fn menu(name: &str, items: Vec<MenuItem>) -> Menu {
    Menu {
        name: name.to_owned().into(),
        items,
        disabled: false,
    }
}
fn menus() -> Vec<Menu> {
    let themes = gpui_ui_kit::theme::ThemeVariant::all()
        .iter()
        .enumerate()
        .map(|(index, theme)| {
            MenuItem::action(
                theme.name(),
                SelectAppearance {
                    theme: Some(index),
                    design: None,
                },
            )
        })
        .collect();
    let designs = [
        "Neutral",
        "Apple",
        "Material 3",
        "Fluent",
        "Adwaita",
        "Breeze",
        "Carbon",
    ]
    .iter()
    .enumerate()
    .map(|(index, name)| {
        MenuItem::action(
            *name,
            SelectAppearance {
                theme: None,
                design: Some(index),
            },
        )
    })
    .collect();
    vec![
        menu(
            "nplank",
            vec![
                MenuItem::action("Preferences…", OpenPreferences),
                MenuItem::action("Pair iOS Device…", PairDevice),
                MenuItem::Separator,
                MenuItem::action("Quit nplank", Quit),
            ],
        ),
        menu(
            "File",
            vec![
                MenuItem::action("New Agent…", NewAgent),
                MenuItem::action("New Workspace…", NewWorkspace),
                MenuItem::Separator,
                MenuItem::action("Close Window", CloseWindow),
            ],
        ),
        menu(
            "Edit",
            vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::Separator,
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ],
        ),
        menu(
            "View",
            vec![
                MenuItem::action("Plank Commands…", OpenCommands),
                MenuItem::action("Fleet Overview", ToggleFleet),
                MenuItem::action("CI Runs", ToggleCi),
                MenuItem::action("Markdown Docs", ToggleDocs),
                MenuItem::action("Monitoring", ToggleMonitor),
                MenuItem::action("Next Needs Attention", NextAttention),
                MenuItem::Submenu(menu("Theme", themes)),
                MenuItem::Submenu(menu("Design", designs)),
                MenuItem::Separator,
                MenuItem::action("Show Archived Agents", ToggleArchived),
            ],
        ),
        menu(
            "Window",
            vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        ),
    ]
}
