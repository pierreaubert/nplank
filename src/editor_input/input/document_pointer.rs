//! Shares an active pointer gesture between rows of one document editor.

use gpui::SharedString;

/// Transient gesture ownership; it contains no document text or saved state.
#[derive(Debug, Default)]
pub(crate) struct DocumentPointer {
    scope: Option<SharedString>,
    page_latched: bool,
}

impl DocumentPointer {
    pub(crate) fn begin(&mut self, scope: SharedString) {
        self.scope = Some(scope);
        self.page_latched = false;
    }

    pub(crate) fn extends(&mut self, scope: &SharedString, left_pressed: bool) -> bool {
        if !left_pressed {
            self.finish();
        }
        let active = self.scope.as_ref() == Some(scope);
        if active {
            self.page_latched = false;
        }
        active
    }

    /// Turns one page per visit to a paging button during a selection gesture.
    pub(crate) fn turn_page(&mut self, scope: &SharedString, left_pressed: bool) -> bool {
        if !left_pressed {
            self.finish();
        }
        if self.scope.as_ref() != Some(scope) || self.page_latched {
            return false;
        }
        self.page_latched = true;
        true
    }

    pub(crate) fn finish(&mut self) {
        self.scope = None;
        self.page_latched = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gestures_cross_rows_only_within_the_active_editor_and_end_on_release() {
        let mut pointer = DocumentPointer::default();
        let first: SharedString = "session-1/revision-1".into();
        let other: SharedString = "session-2/revision-1".into();
        assert!(!pointer.extends(&first, true));
        pointer.begin(first.clone());
        assert!(pointer.extends(&first, true));
        assert!(!pointer.extends(&other, true));
        assert!(!pointer.extends(&first, false));
        assert!(!pointer.extends(&first, true));
        pointer.begin(other.clone());
        pointer.finish();
        assert!(!pointer.extends(&other, true));
    }

    #[test]
    fn page_hover_extends_a_unicode_selection_once_per_visit_and_cut_is_undoable() {
        use crate::document::{Document, PAGE_LINES};
        let mut document = Document::new(&vec!["a🦀é"; PAGE_LINES + 2].join("\n"));
        let original = document.text();
        let scope: SharedString = "editor".into();
        let mut pointer = DocumentPointer::default();
        pointer.begin(scope.clone());
        let anchor = document.lines.get(PAGE_LINES - 1).unwrap().id.clone();
        document.observe_selection(&anchor, 1, 1, false);
        assert!(pointer.turn_page(&scope, true));
        document.page += 1;
        assert!(
            !pointer.turn_page(&scope, true),
            "Hovering must not race through pages"
        );
        assert!(pointer.extends(&scope, true));
        let head = document.lines.get(PAGE_LINES + 1).unwrap().id.clone();
        document.extend_to(&head, 2);
        assert_eq!(document.selected_text().as_deref(), Some("🦀é\na🦀é\na🦀"));
        assert_eq!(document.page, 1);
        assert!(document.replace_selection(""));
        document.undo();
        assert_eq!(document.text(), original);
        // Returning to a line rearms page hover; release elsewhere retires it.
        assert!(pointer.turn_page(&scope, true));
        pointer.finish();
        assert!(!pointer.turn_page(&scope, true));
        assert!(!pointer.extends(&scope, true));
    }
}
