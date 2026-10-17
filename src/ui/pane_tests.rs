//! Native pane focus, composition, and layout regressions.

use super::*;
use gpui::{TestAppContext, VisualTestContext};

#[gpui::test]
async fn splits_keep_keyboard_and_drafts_in_the_focused_pane(cx: &mut TestAppContext) {
    let temporary = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(&temporary.path().join("test.sqlite")).unwrap();
    let workspace = store
        .create_workspace("Test", temporary.path().to_str().unwrap())
        .unwrap();
    let first = store
        .create_session(&workspace, "First", Backend::Demo)
        .unwrap();
    let second = store
        .create_session(
            &workspace,
            "Second",
            Backend::Local {
                executable: "plank".into(),
                args: vec![],
            },
        )
        .unwrap();
    let snapshot = store.snapshot(None, 0).unwrap();
    let window = cx.add_window(|window, cx| {
        // Appearance initialization avoids the root's network bridge. The child bridge
        // has an unreachable loopback address and cannot affect any real daemon.
        let mut view = WorkspaceView::with_preferences(
            Client::new("http://127.0.0.1:1", "test".into()).unwrap(),
            temporary.path().to_owned(),
            true,
            Preferences {
                selected: Some(first.clone()),
                ..Default::default()
            },
            cx,
        );
        view.appearance = false;
        view.appearance_only = false;
        view.snapshot = snapshot;
        view.composer_focus.focus(window, cx);
        view
    });
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.simulate_input("first draft");
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-alt-v"
    } else {
        "ctrl-alt-v"
    });
    cx.run_until_parked();
    let child = window
        .update(&mut cx, |view, window, cx| {
            assert_eq!(
                view.drafts.get(&first).map(String::as_str),
                Some("first draft")
            );
            assert_eq!(view.pane_splits.len(), 1);
            let child = view.pane_splits[0].1.clone();
            child.update(cx, |pane, cx| {
                assert_eq!(pane.selected.as_deref(), Some(second.as_str()));
                pane.composer_focus.focus(window, cx);
            });
            child
        })
        .unwrap();
    cx.simulate_input("/mo");
    cx.simulate_keystrokes("tab");
    cx.run_until_parked();
    window
        .update(&mut cx, |view, _, cx| {
            assert_eq!(
                view.drafts.get(&first).map(String::as_str),
                Some("first draft")
            );
            let pane = child.read(cx);
            let draft = pane.drafts.get(&second).unwrap();
            assert!(draft.starts_with("/model"), "slash completion: {draft}");
        })
        .unwrap();
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-alt-h"
    } else {
        "ctrl-alt-h"
    });
    cx.run_until_parked();
    window
        .update(&mut cx, |view, _, cx| {
            assert_eq!(
                view.pane_splits.len(),
                1,
                "child shortcut must not also split the parent"
            );
            assert_eq!(child.read(cx).pane_splits.len(), 1);
            assert!(child.read(cx).pane_size.unwrap().width < view.pane_size.unwrap().width);
        })
        .unwrap();
}
