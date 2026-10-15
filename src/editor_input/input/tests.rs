use super::{EditState, Input, InputEntity};
use gpui::SharedString;
use std::cell::RefCell;

#[test]
fn native_composition_crossing_rows_rebases_the_input_before_focus_handoff() {
    use crate::document::{Document, NativeEdit};
    for reverse in [false, true] {
        let mut document = Document::new("ab🦀\nmiddle\nlast!");
        let original = document.lines.clone();
        let first = document.lines.get(0).unwrap().id.clone();
        let last = document.lines.get(2).unwrap().id.clone();
        let (source, column) = if reverse {
            document.observe_selection(&last, 2, 2, false);
            document.extend_to(&first, 1);
            (first.clone(), 1)
        } else {
            document.observe_selection(&first, 1, 1, false);
            document.extend_to(&last, 2);
            (last.clone(), 2)
        };
        let source_text = document
            .lines
            .iter()
            .find(|line| line.id == source)
            .unwrap()
            .text
            .clone();
        let mut input = EditState::new(&source_text);
        input.cursor = column;
        input.selection_anchor = None;
        for text in ["n", "に", "日本🦀"] {
            let range = InputEntity::native_range(&input, None);
            InputEntity::replace_native_text(&mut input, None, text, true, None);
            let result = document
                .native_edit(
                    &source,
                    NativeEdit::Replace {
                        range,
                        text,
                        composing: true,
                        selected: None,
                    },
                )
                .unwrap();
            assert_eq!(result.line_id, first);
            InputEntity::apply_native_state(&mut input, result.clone());
            assert_eq!(input.text, document.lines.get(0).unwrap().text);
            assert_eq!(input.marked_range, Some(1..1 + text.chars().count()));
            assert_eq!(
                document.navigation.as_ref().unwrap().marked,
                input.marked_range
            );
            assert_eq!(document.text(), format!("a{text}st!"));
            if source != first {
                document.native_edit_end(&source);
            }
        }
        // After the repaint, the surviving row owns subsequent native callbacks.
        let range = InputEntity::native_range(&input, None);
        InputEntity::replace_native_text(&mut input, None, "日本", false, None);
        let result = document
            .native_edit(
                &first,
                NativeEdit::Replace {
                    range,
                    text: "日本",
                    composing: false,
                    selected: None,
                },
            )
            .unwrap();
        InputEntity::apply_native_state(&mut input, result);
        assert_eq!(document.text(), "a日本st!");
        assert!(input.marked_range.is_none());
        document.undo();
        assert_eq!(document.lines, original);
        document.redo();
        assert_eq!(document.text(), "a日本st!");
    }
}

#[test]
fn native_document_handoff_handles_paste_unmark_and_clicking_the_original_row() {
    use crate::document::{Document, NativeEdit};
    let mut document = Document::new("abc\ndef");
    let first = document.lines.get(0).unwrap().id.clone();
    document.observe_selection(&first, 1, 1, false);
    let result = document
        .native_edit(
            &first,
            NativeEdit::Replace {
                range: 1..1,
                text: "X\nY",
                composing: false,
                selected: None,
            },
        )
        .unwrap();
    assert_eq!(document.text(), "aX\nYbc\ndef");
    assert_ne!(result.line_id, first);
    assert_eq!(result.text, "Ybc");
    assert_eq!(result.selected, 1..1);
    // An ordinary click returns ownership to the clicked row, retiring aliases.
    document.observe_selection(&first, 0, 0, false);
    let result = document
        .native_edit(
            &first,
            NativeEdit::Replace {
                range: 0..0,
                text: "secret",
                composing: true,
                selected: None,
            },
        )
        .unwrap();
    assert_eq!(document.lines.get(0).unwrap().text, "secretaX");
    assert!(!format!("{result:?}").contains("secret"));
    assert!(result.marked.is_some());
    let result = document.native_edit(&first, NativeEdit::Unmark).unwrap();
    assert!(result.marked.is_none());
    assert!(document.navigation.as_ref().unwrap().marked.is_none());
    let saved = serde_json::to_value(&document).unwrap();
    assert!(saved.get("native").is_none());
    assert!(saved.get("native_source").is_none());
    assert!(saved.get("navigation").is_none());
}

#[test]
fn document_composition_undo_does_not_consume_typing_before_the_preedit() {
    use crate::document::{Document, NativeEdit};
    let mut document = Document::new("abc");
    let id = document.lines.get(0).unwrap().id.clone();
    document.set(&id, "abcd");
    document
        .native_edit(
            &id,
            NativeEdit::Replace {
                range: 4..4,
                text: "n",
                composing: true,
                selected: None,
            },
        )
        .unwrap();
    document
        .native_edit(
            &id,
            NativeEdit::Replace {
                range: 4..5,
                text: "你",
                composing: true,
                selected: None,
            },
        )
        .unwrap();
    document
        .native_edit(
            &id,
            NativeEdit::Replace {
                range: 4..5,
                text: "你",
                composing: false,
                selected: None,
            },
        )
        .unwrap();
    document.undo();
    assert_eq!(document.text(), "abcd");
    document.undo();
    assert_eq!(document.text(), "abc");
}

#[test]
fn native_ime_replaces_preedit_and_commits_as_one_undoable_edit_after_an_emoji() {
    let mut state = EditState::new("🦀 []");
    state.cursor = 3;
    state.selection_anchor = None;
    InputEntity::replace_native_text(&mut state, None, "n", true, Some(1..1));
    assert_eq!(state.text, "🦀 [n]");
    assert_eq!(state.marked_range, Some(3..4));
    assert!(InputEntity::selection_from_state(&state).composing);
    InputEntity::replace_native_text(&mut state, None, "に", true, Some(1..1));
    assert_eq!(state.text, "🦀 [に]");
    InputEntity::replace_native_text(&mut state, None, "日本🦀", true, Some(2..4));
    assert_eq!(state.text, "🦀 [日本🦀]");
    assert_eq!(state.marked_range, Some(3..6));
    assert_eq!(state.selection_range(), Some((5, 6)));
    assert_eq!(InputEntity::char_to_utf16(&state.text, state.cursor), 8);
    InputEntity::replace_native_text(&mut state, None, "日本", false, None);
    assert_eq!(state.text, "🦀 [日本]");
    assert!(state.marked_range.is_none());
    assert!(!InputEntity::selection_from_state(&state).composing);
    assert!(state.undo());
    assert_eq!(state.text, "🦀 []");
    assert!(state.marked_range.is_none());
    assert!(state.redo());
    assert_eq!(state.text, "🦀 [日本]");
    assert!(state.marked_range.is_none());
}

#[test]
fn native_ime_explicit_utf16_ranges_and_empty_preedit_preserve_surrounding_text() {
    let mut state = EditState::new("🦀ABCDE");
    InputEntity::replace_native_text(&mut state, Some(3..5), "中", true, Some(0..1));
    assert_eq!(state.text, "🦀A中DE");
    assert_eq!(state.marked_range, Some(2..3));
    assert_eq!(state.selection_range(), Some((2, 3)));
    InputEntity::replace_native_text(&mut state, None, "", true, Some(0..0));
    assert_eq!(state.text, "🦀ADE");
    assert!(state.marked_range.is_none());
    assert_eq!(state.cursor, 2);
}

#[test]
fn native_ime_marks_survive_matching_parent_updates_and_retire_on_navigation_or_replacement() {
    let mut state = EditState::new("prefix");
    state.cursor = 6;
    state.selection_anchor = None;
    InputEntity::replace_native_text(&mut state, None, "かな", true, None);
    state.sync_external_value("prefix", "prefixかな");
    assert_eq!(state.marked_range, Some(6..8));
    state.navigate(1, 0..0);
    assert!(state.marked_range.is_none());
    InputEntity::replace_native_text(&mut state, None, "仮", true, None);
    state.sync_external_value("prefixかな", "replacement");
    assert!(state.marked_range.is_none());
    InputEntity::replace_native_text(&mut state, None, "secret-preedit", true, None);
    assert!(state.marked_range.is_some());
    assert!(!format!("{state:?}").contains("secret-preedit"));
    state.finish_edit();
    assert!(state.marked_range.is_none());
    assert!(state.text.is_empty());
}

#[test]
fn document_navigation_selects_unicode_scalars_once_without_changing_text() {
    let mut state = EditState::new("a🦀éz");
    state.navigate(1, 1..3);
    assert_eq!(state.selection_range(), Some((1, 3)));
    assert_eq!(InputEntity::char_to_utf16(&state.text, state.cursor), 4);
    assert_eq!(state.text, "a🦀éz");
    state.cursor = 2;
    state.selection_anchor = None;
    state.navigate(1, 1..3);
    assert_eq!(
        state.cursor, 2,
        "a redraw must preserve manual cursor movement"
    );
    state.navigate(2, 0..0);
    assert_eq!(state.cursor, 0);
    assert!(state.selection_range().is_none());
    state.navigate(3, 1..3);
    assert!(state.delete_selection());
    assert_eq!(state.text, "az");
}

#[test]
fn document_input_cache_retains_only_visible_line_state() {
    use super::{EDIT_STATES, TEXT_ORIGINS, cleanup_stale_input_states};
    let ids = (0..100)
        .map(|n| gpui::ElementId::from(SharedString::from(format!("line-{n}"))))
        .collect::<Vec<_>>();
    for id in &ids {
        EDIT_STATES.with(|states| {
            states.borrow_mut().insert(
                id.clone(),
                std::rc::Rc::new(RefCell::new(EditState::new("line"))),
            );
        });
        TEXT_ORIGINS.with(|origins| {
            origins.borrow_mut().insert(id.clone(), 0.0);
        });
    }
    let retained = ids
        .into_iter()
        .take(12)
        .collect::<std::collections::HashSet<_>>();
    cleanup_stale_input_states(&retained);
    assert_eq!(EDIT_STATES.with(|states| states.borrow().len()), 12);
    assert_eq!(TEXT_ORIGINS.with(|origins| origins.borrow().len()), 12);
    cleanup_stale_input_states(&std::collections::HashSet::<gpui::ElementId>::new());
    assert_eq!(EDIT_STATES.with(|states| states.borrow().len()), 0);
}

#[test]
fn password_builder_enables_masking() {
    let input = Input::new("password").value("secret").password(true);
    assert!(input.password);
    assert_eq!(input.value, SharedString::from("secret"));
}

#[test]
fn password_mask_cache_keeps_only_character_count() {
    let cache = RefCell::new(None);
    let mask = InputEntity::cached_password_mask(&cache, "sëcrèt");

    assert_eq!(mask, SharedString::from("••••••"));
    assert_eq!(cache.borrow().as_ref().map(|(count, _)| *count), Some(6));
}

#[test]
fn password_input_debug_output_redacts_the_value() {
    let input = Input::new("password")
        .value("not-for-debug-output")
        .password(true);

    let dump = format!("{input:?}");
    assert!(dump.contains("<redacted>"));
    assert!(!dump.contains("not-for-debug-output"));
}

#[test]
fn effective_label_prefers_aria_label_then_label_then_placeholder() {
    let input = Input::new("a").aria_label("aria");
    assert_eq!(input.aria_label, Some(SharedString::from("aria")));
    assert!(input.label.is_none());
    assert!(input.placeholder.is_none());

    let input = Input::new("b").label("label").placeholder("placeholder");
    assert!(input.aria_label.is_none());
    assert_eq!(input.label, Some(SharedString::from("label")));
    assert_eq!(input.placeholder, Some(SharedString::from("placeholder")));

    let input = Input::new("c").placeholder("placeholder");
    assert!(input.aria_label.is_none());
    assert!(input.label.is_none());
    assert_eq!(input.placeholder, Some(SharedString::from("placeholder")));
}

#[test]
fn insert_char_does_not_allocate_string_and_matches_insert_text() {
    let mut state = EditState::new("ab");
    state.clear_selection();
    state.cursor = 1;

    let mut via_char = state.clone();
    via_char.insert_char('X');

    let mut via_text = state.clone();
    via_text.insert_text("X");

    assert_eq!(via_char.text, "aXb");
    assert_eq!(via_text.text, "aXb");
    assert_eq!(via_char.cursor, 2);
    assert_eq!(via_text.cursor, 2);
}

#[test]
fn validator_reports_error_message_for_invalid_value() {
    let input = Input::new("email").validator(|value| {
        if value.contains('@') {
            None
        } else {
            Some(SharedString::from("Enter a valid email address"))
        }
    });

    assert_eq!(input.validate("user@example.com"), None);
    assert_eq!(
        input.validate("not-an-email"),
        Some(SharedString::from("Enter a valid email address"))
    );
}

#[test]
fn effective_error_prefers_explicit_error_over_validator() {
    let without_validator = Input::new("plain");
    assert_eq!(without_validator.validate("anything"), None);
    assert_eq!(without_validator.effective_error(), None);

    let input = Input::new("email")
        .value("not-an-email")
        .error("Server rejected this value")
        .validator(|value| {
            if value.contains('@') {
                None
            } else {
                Some(SharedString::from("Enter a valid email address"))
            }
        });
    // Explicit error wins even though the validator also fails.
    assert_eq!(
        input.effective_error(),
        Some(SharedString::from("Server rejected this value"))
    );

    let validated = Input::new("email")
        .value("not-an-email")
        .validator(|_| Some(SharedString::from("bad")));
    assert_eq!(validated.effective_error(), Some(SharedString::from("bad")));
}

#[test]
fn external_values_update_active_edits_without_erasing_uncommitted_typing() {
    let mut state = EditState::default();
    state.editing = true;
    state.text = "typing".into();
    state.cursor = 6;
    state.sync_external_value("saved", "saved");
    assert_eq!(state.text, "typing");
    state.sync_external_value("saved", "é");
    assert_eq!(state.text, "é");
    assert_eq!(state.cursor, 1);
    state.selection_anchor = Some(0);
    state.sync_external_value("é", "é");
    assert_eq!(state.selection_anchor, Some(0));
}
