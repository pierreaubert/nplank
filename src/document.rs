//! Portable document drafts for compact, toolkit-native editing forms.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Serialize};

/// One editable line with a stable toolkit input identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Line {
    pub id: String,
    pub text: String,
}

/// An unsaved document, retained independently of the engine's original text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Document {
    #[serde(skip)]
    pub navigation: Option<Navigation>,
    #[serde(skip)]
    pub search: Search,
    pub lines: Vec<Line>,
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    undo: Vec<Change>,
    #[serde(default)]
    redo: Vec<Change>,
}

/// Transient literal search state; it never changes saved document content.
#[derive(Debug, Clone, Default)]
pub(crate) struct Search {
    pub open: bool,
    pub query: String,
    pub replacement: String,
    offset: Option<usize>,
}

/// One transient cursor request for a stable line identity.
#[derive(Debug, Clone)]
pub(crate) struct Navigation {
    pub line_id: String,
    pub range: std::ops::Range<usize>,
    pub token: u128,
}

// Limit persisted history independently of file size and the number of keystrokes.
const HISTORY_BYTES: usize = 4 * 1024 * 1024;
const HISTORY_CHANGES: usize = 128;
pub(crate) const PAGE_LINES: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Change {
    start: usize,
    before: Vec<Line>,
    after: Vec<Line>,
    page: usize,
    #[serde(skip)]
    typing: Option<String>,
}

impl Change {
    fn bytes(&self) -> usize {
        self.before
            .iter()
            .chain(&self.after)
            .map(|line| line.text.len() + line.id.len())
            .sum()
    }
}

impl Document {
    pub fn new(text: &str) -> Self {
        Self {
            navigation: None,
            search: Search::default(),
            page: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            lines: text
                .split('\n')
                .map(|text| Line {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: text.into(),
                })
                .collect(),
        }
    }

    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn search_for(&mut self, query: &str) {
        self.search.open = true;
        self.search.query = query.into();
        self.search.offset = None;
        self.find(false);
        // Typing a query keeps focus in the find field. Explicit navigation
        // selects the result, without making the search field lose keystrokes.
        self.navigation = None;
    }

    pub fn find(&mut self, backwards: bool) {
        if self.search.query.is_empty() {
            return;
        }
        self.end_typing();
        let text = self.text();
        let matches = || {
            text.match_indices(&self.search.query)
                .map(|(offset, _)| offset)
        };
        let found = if backwards {
            matches()
                .take_while(|offset| self.search.offset.is_some_and(|current| *offset < current))
                .last()
                .or_else(|| matches().last())
        } else {
            matches()
                .find(|offset| self.search.offset.is_none_or(|current| *offset > current))
                .or_else(|| matches().next())
        };
        self.search.offset = found;
        if let Some(offset) = self.search.offset {
            let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count();
            self.page = line / PAGE_LINES;
            let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
            let from = text[start..offset].chars().count();
            // A multi-line query selects its first line segment; inputs remain
            // line-based so cross-line selection is not implied.
            let end = (offset + self.search.query.len()).min(start + self.lines[line].text.len());
            let to = text[start..end].chars().count();
            self.navigation = Some(Navigation {
                line_id: self.lines[line].id.clone(),
                range: from..to,
                token: uuid::Uuid::new_v4().as_u128(),
            });
        } else {
            self.navigation = None;
        }
    }

    pub fn search_summary(&self) -> (Option<usize>, usize, Option<usize>) {
        if self.search.query.is_empty() {
            return (None, 0, None);
        }
        let text = self.text();
        let mut total = 0;
        let mut current = None;
        let mut line = None;
        for (offset, _) in text.match_indices(&self.search.query) {
            total += 1;
            if self.search.offset == Some(offset) {
                current = Some(total);
                line = Some(text[..offset].bytes().filter(|byte| *byte == b'\n').count());
            }
        }
        (current, total, line)
    }

    /// Replaces the current literal match as one undoable document change.
    pub fn replace_match(&mut self) -> bool {
        let Some(offset) = self.search.offset else {
            return false;
        };
        let text = self.text();
        let end = offset.saturating_add(self.search.query.len());
        if self.search.query.is_empty()
            || self.search.query == self.search.replacement
            || text.get(offset..end) != Some(self.search.query.as_str())
        {
            return false;
        }
        // Match offsets refer to UTF-8 boundaries, including matches across pages.
        let first = text[..offset].bytes().filter(|byte| *byte == b'\n').count();
        let last = text[..end].bytes().filter(|byte| *byte == b'\n').count();
        let start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
        let tail = text[end..]
            .find('\n')
            .map_or(text.len(), |index| end + index);
        let edited = format!(
            "{}{}{}",
            &text[start..offset],
            self.search.replacement,
            &text[end..tail]
        );
        let before = self.lines[first..=last].to_vec();
        let after: Vec<_> = edited
            .split('\n')
            .enumerate()
            .map(|(index, text)| Line {
                id: if index == 0 {
                    before[0].id.clone()
                } else {
                    uuid::Uuid::new_v4().to_string()
                },
                text: text.into(),
            })
            .collect();
        self.end_typing();
        self.lines.splice(first..=last, after.clone());
        self.record(Change {
            start: first,
            before,
            after,
            page: self.page,
            typing: None,
        });
        self.page = self.page.min(self.pages() - 1);
        // Continue after inserted text rather than selecting it again immediately.
        self.search.offset = offset
            .checked_add(self.search.replacement.len())
            .and_then(|end| end.checked_sub(1));
        self.find(false);
        true
    }

    pub fn set(&mut self, id: &str, text: &str) {
        if let Some(index) = self.lines.iter().position(|line| line.id == id) {
            if self.lines[index].text == text {
                return;
            }
            let before = vec![self.lines[index].clone()];
            let mut parts = text.split('\n');
            self.lines[index].text = parts.next().unwrap_or_default().into();
            let following: Vec<_> = parts
                .map(|text| Line {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: text.into(),
                })
                .collect();
            let count = following.len() + 1;
            self.lines.splice(index + 1..index + 1, following);
            self.record(Change {
                start: index,
                before,
                after: self.lines[index..index + count].to_vec(),
                page: self.page,
                typing: (count == 1).then(|| id.to_owned()),
            });
        }
    }

    pub fn insert_after(&mut self, id: &str) {
        if let Some(index) = self.lines.iter().position(|line| line.id == id) {
            self.end_typing();
            self.lines.insert(
                index + 1,
                Line {
                    id: uuid::Uuid::new_v4().to_string(),
                    text: String::new(),
                },
            );
            self.record(Change {
                start: index + 1,
                before: Vec::new(),
                after: vec![self.lines[index + 1].clone()],
                page: self.page,
                typing: None,
            });
        }
    }

    pub fn remove(&mut self, id: &str) {
        let Some(index) = self.lines.iter().position(|line| line.id == id) else {
            return;
        };
        self.end_typing();
        if self.lines.len() == 1 {
            self.set(id, "");
            self.end_typing();
        } else {
            let before = vec![self.lines.remove(index)];
            self.record(Change {
                start: index,
                before,
                after: Vec::new(),
                page: self.page,
                typing: None,
            });
            self.page = self.page.min(self.pages() - 1);
        }
    }

    pub fn pages(&self) -> usize {
        self.lines.len().div_ceil(PAGE_LINES).max(1)
    }

    pub fn go_to_line(&mut self, line: usize) -> bool {
        if line == 0 || line > self.lines.len() {
            return false;
        }
        self.end_typing();
        self.page = (line - 1) / PAGE_LINES;
        self.navigation = Some(Navigation {
            line_id: self.lines[line - 1].id.clone(),
            range: 0..0,
            token: uuid::Uuid::new_v4().as_u128(),
        });
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) {
        self.navigation = None;
        self.end_typing();
        if let Some(mut change) = self.undo.pop() {
            self.search.offset = None;
            if self
                .lines
                .get(change.start..change.start.saturating_add(change.after.len()))
                != Some(change.after.as_slice())
            {
                self.undo.clear();
                self.redo.clear();
                return;
            }
            self.lines.splice(
                change.start..change.start + change.after.len(),
                change.before.clone(),
            );
            self.page = change.page.min(self.pages() - 1);
            change.typing = None;
            self.redo.push(change);
        }
    }

    pub fn redo(&mut self) {
        self.navigation = None;
        self.end_typing();
        if let Some(change) = self.redo.pop() {
            self.search.offset = None;
            if self
                .lines
                .get(change.start..change.start.saturating_add(change.before.len()))
                != Some(change.before.as_slice())
            {
                self.undo.clear();
                self.redo.clear();
                return;
            }
            self.lines.splice(
                change.start..change.start + change.before.len(),
                change.after.clone(),
            );
            self.page = (change.start / PAGE_LINES).min(self.pages() - 1);
            self.undo.push(change);
        }
    }

    pub fn end_typing(&mut self) {
        if let Some(change) = self.undo.last_mut() {
            change.typing = None;
        }
    }

    fn record(&mut self, change: Change) {
        self.navigation = None;
        self.search.offset = None;
        self.redo.clear();
        if let Some(previous) = self.undo.last_mut()
            && change.typing.is_some()
            && previous.typing == change.typing
            && previous.start == change.start
        {
            previous.after = change.after;
        } else {
            self.undo.push(change);
        }
        let mut bytes: usize = self.undo.iter().map(Change::bytes).sum();
        while self.undo.len() > HISTORY_CHANGES || bytes > HISTORY_BYTES {
            bytes = bytes.saturating_sub(self.undo.remove(0).bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn replacement_spans_lines_and_pages_and_undo_restores_line_ids() {
        let original = format!("{}a🦀\néz\na🦀\néz\ntail", "blank\n".repeat(PAGE_LINES - 1));
        let mut document = super::Document::new(&original);
        let ids = document.lines.clone();
        document.search_for("🦀\né");
        document.search.replacement = "new\nvalue\n".into();
        assert!(document.replace_match());
        assert_eq!(
            document.text(),
            original.replacen("🦀\né", "new\nvalue\n", 1)
        );
        assert_eq!(document.search_summary().1, 1);
        assert_eq!(document.lines.last().unwrap().id, ids.last().unwrap().id);
        document.undo();
        assert_eq!(document.lines, ids);
        document.redo();
        assert_eq!(
            document.text(),
            original.replacen("🦀\né", "new\nvalue\n", 1)
        );
        let recovered: super::Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(recovered.search.replacement.is_empty());
    }

    #[test]
    fn replacement_deletes_newlines_and_refuses_stale_or_unchanged_matches() {
        let mut document = super::Document::new("one\ntwo\n");
        document.search_for("\n");
        assert!(document.replace_match());
        assert_eq!(document.text(), "onetwo\n");
        assert!(document.replace_match());
        assert_eq!(document.text(), "onetwo");
        assert!(!document.replace_match());
        document.search_for("one");
        document.search.replacement = "one".into();
        assert!(!document.replace_match());
        document.search.replacement = "changed".into();
        let id = document.lines[0].id.clone();
        document.set(&id, "new content");
        assert!(!document.replace_match());
        assert_eq!(document.text(), "new content");
    }
    #[test]
    fn navigation_uses_unicode_scalars_and_never_persists_or_edits_content() {
        let mut document = super::Document::new("a🦀éz\nsecond line");
        document.search_for("🦀é");
        assert!(
            document.navigation.is_none(),
            "typing a query keeps search focus"
        );
        document.find(false);
        let navigation = document.navigation.as_ref().unwrap();
        assert_eq!(navigation.line_id, document.lines[0].id);
        assert_eq!(navigation.range, 1..3);
        let token = navigation.token;
        document.find(false);
        assert_ne!(document.navigation.as_ref().unwrap().token, token);
        assert!(document.go_to_line(2));
        assert_eq!(document.navigation.as_ref().unwrap().range, 0..0);
        assert_eq!(
            document.navigation.as_ref().unwrap().line_id,
            document.lines[1].id
        );
        let restored: super::Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(restored.navigation.is_none());
        assert_eq!(restored.text(), "a🦀éz\nsecond line");
        let id = document.lines[1].id.clone();
        document.set(&id, "changed");
        assert!(document.navigation.is_none());
    }
    #[test]
    fn search_wraps_across_pages_and_unicode_lines_without_editing_content() {
        let original = format!("{}café 🦀\nnext 🦀\n", "blank\n".repeat(PAGE_LINES));
        let mut document = Document::new(&original);
        document.search_for("🦀");
        assert_eq!(document.search_summary(), (Some(1), 2, Some(PAGE_LINES)));
        assert_eq!(document.page, 1);
        document.find(true);
        assert_eq!(
            document.search_summary(),
            (Some(2), 2, Some(PAGE_LINES + 1))
        );
        document.find(false);
        assert_eq!(document.search_summary().0, Some(1));
        document.search_for("café 🦀\nnext");
        assert_eq!(document.search_summary(), (Some(1), 1, Some(PAGE_LINES)));
        document.search_for("CAFÉ");
        assert_eq!(document.search_summary(), (None, 0, None));
        document.find(true);
        document.search_for("");
        assert_eq!(document.search_summary(), (None, 0, None));
        assert_eq!(document.text(), original);
        assert!(!document.can_undo());
    }

    #[test]
    fn search_cursors_are_invalidated_by_edits_and_not_saved_in_drafts() {
        let mut document = Document::new("first match\nsecond match");
        document.search_for("match");
        let id = document.lines[0].id.clone();
        document.set(&id, "changed");
        assert_eq!(document.search_summary(), (None, 1, None));
        document.find(false);
        assert_eq!(document.search_summary(), (Some(1), 1, Some(1)));
        document.undo();
        assert_eq!(document.search_summary(), (None, 2, None));
        document.find(false);
        document.redo();
        assert_eq!(document.search_summary(), (None, 1, None));
        let restored: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(!restored.search.open);
        assert!(restored.search.query.is_empty());
        assert_eq!(restored.text(), "changed\nsecond match");
    }
    use super::*;

    #[test]
    fn document_preserves_unicode_line_endings_and_stable_input_identity() {
        for text in ["", "hello", "hello\n", "café\r\n🦀\n"] {
            assert_eq!(Document::new(text).text(), text);
        }
        let mut document = Document::new("first\nlast\n");
        let first = document.lines[0].id.clone();
        let last = document.lines[1].id.clone();
        document.insert_after(&first);
        assert_eq!(document.lines[2].id, last);
        let inserted = document.lines[1].id.clone();
        document.set(&inserted, "middle 🦀");
        assert_eq!(document.text(), "first\nmiddle 🦀\nlast\n");
        document.remove(&inserted);
        assert_eq!(document.text(), "first\nlast\n");
        let saved = serde_json::to_string(&document).unwrap();
        assert_eq!(
            serde_json::from_str::<Document>(&saved).unwrap().text(),
            document.text()
        );
        document.set(&first, "one\ntwo\nthree");
        assert_eq!(document.text(), "one\ntwo\nthree\nlast\n");
        assert_eq!(document.lines[3].id, last);
    }

    #[test]
    fn undo_groups_typing_and_restores_pastes_and_line_identity() {
        let mut document = Document::new("first\nlast\n");
        let first = document.lines[0].id.clone();
        let last = document.lines[1].id.clone();
        document.set(&first, "f");
        document.set(&first, "foo 🦀");
        document.undo();
        assert_eq!(document.text(), "first\nlast\n");
        assert!(!document.can_undo());
        document.redo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
        document.set(&first, "one\ntwo\nthree");
        let inserted = document.lines[1].id.clone();
        document.undo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
        document.redo();
        assert_eq!(document.lines[1].id, inserted);
        assert_eq!(document.lines[3].id, last);
        document.remove(&last);
        document.undo();
        assert_eq!(document.lines[3].id, last);
        document.undo();
        document.set(&first, "alternative");
        assert!(!document.can_redo());
        document.undo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
    }

    #[test]
    fn history_survives_recovery_without_merging_a_new_edit() {
        let mut document = Document::new("original");
        let id = document.lines[0].id.clone();
        document.set(&id, "saved draft");
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        recovered.set(&id, "new edit");
        recovered.undo();
        assert_eq!(recovered.text(), "saved draft");
        recovered.undo();
        assert_eq!(recovered.text(), "original");
        let mut older: Document =
            serde_json::from_value(serde_json::json!({"lines":[{"id":"old","text":"legacy"}]}))
                .unwrap();
        assert_eq!(older.text(), "legacy");
        assert!(!older.can_undo());
        older.remove("old");
        older.undo();
        assert_eq!(older.text(), "legacy");
    }

    #[test]
    fn navigation_and_history_are_bounded_and_corrupt_history_does_not_change_text() {
        let mut document = Document::new(&vec!["line"; 25].join("\n"));
        assert_eq!(document.pages(), 3);
        assert!(!document.go_to_line(0));
        assert!(!document.go_to_line(26));
        assert!(document.go_to_line(25));
        assert_eq!(document.page, 2);
        let id = document.lines[0].id.clone();
        for index in 0..HISTORY_CHANGES + 1 {
            document.end_typing();
            document.set(&id, &index.to_string());
        }
        assert_eq!(document.undo.len(), HISTORY_CHANGES);
        let mut large = Document::new("small");
        let id = large.lines[0].id.clone();
        for index in 0..30 {
            large.end_typing();
            large.set(&id, &format!("{index}{}", "x".repeat(256 * 1024)));
        }
        assert!(large.undo.iter().map(Change::bytes).sum::<usize>() <= HISTORY_BYTES);
        let original = large.text();
        large.undo.last_mut().unwrap().start = usize::MAX;
        large.undo();
        assert_eq!(large.text(), original);
        assert!(!large.can_undo());
    }
}
