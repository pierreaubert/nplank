//! Portable document drafts for compact, toolkit-native editing forms.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Serialize};

mod history;
mod lines;
use history::TextChange;
use lines::Lines;

/// An unsaved document, retained independently of the engine's original text.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "StoredDocument")]
pub(crate) struct Document {
    #[serde(skip)]
    native: Option<NativeState>,
    #[serde(skip)]
    native_source: Option<String>,
    #[serde(skip)]
    pub selection: Option<Selection>,
    #[serde(skip)]
    preferred_column: Option<usize>,
    #[serde(skip)]
    ignore_selection_once: Option<String>,
    #[serde(skip)]
    pub navigation: Option<Navigation>,
    #[serde(skip)]
    pub search: Search,
    pub lines: Lines,
    #[serde(default)]
    pub page: usize,
    #[serde(default)]
    undo: Vec<Change>,
    #[serde(default)]
    redo: Vec<Change>,
}

// Keep the persisted schema compatible while validating recovered history.
#[derive(Deserialize)]
struct StoredDocument {
    lines: Lines,
    #[serde(default)]
    page: usize,
    #[serde(default)]
    undo: Vec<Change>,
    #[serde(default)]
    redo: Vec<Change>,
}

impl From<StoredDocument> for Document {
    fn from(stored: StoredDocument) -> Self {
        let mut document = Self::from_lines(stored.lines);
        document.page = stored.page.min(document.pages() - 1);
        document.undo = stored.undo;
        document.redo = stored.redo;
        document.trim_history();
        document
    }
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
    pub marked: Option<std::ops::Range<usize>>,
    pub line_id: String,
    pub range: std::ops::Range<usize>,
    pub token: u128,
}

/// One native replacement, expressed in Unicode scalar columns.
pub(crate) enum NativeEdit<'a> {
    Replace {
        range: std::ops::Range<usize>,
        text: &'a str,
        composing: bool,
        selected: Option<std::ops::Range<usize>>,
    },
    Unmark,
}

/// Canonical row state after a native edit, including a possible focus handoff.
#[derive(Clone)]
pub(crate) struct NativeState {
    pub line_id: String,
    pub text: String,
    pub selected: std::ops::Range<usize>,
    pub marked: Option<std::ops::Range<usize>>,
    pub token: u128,
}

impl std::fmt::Debug for NativeState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeState")
            .field("line_id", &self.line_id)
            .field("text", &"<redacted>")
            .field("selected", &self.selected)
            .field("marked", &self.marked)
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for NativeEdit<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unmark => formatter.write_str("NativeEdit::Unmark"),
            Self::Replace {
                range, composing, ..
            } => formatter
                .debug_struct("NativeEdit::Replace")
                .field("range", range)
                .field("composing", composing)
                .finish_non_exhaustive(),
        }
    }
}

/// A document cursor measured in lines and Unicode scalar columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Position {
    pub line: usize,
    pub column: usize,
}

/// Transient selection endpoints, retaining the direction of extension.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Selection {
    pub anchor: Position,
    pub head: Position,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Movement {
    Up,
    Down,
    Left,
    Right,
    Start,
    End,
}

// Limit retained history allocations independently of file size and keystrokes.
// Count shared chunks conservatively, including retained originals and capacity.
const HISTORY_BYTES: usize = 4 * 1024 * 1024;
const HISTORY_CHANGES: usize = 128;
pub(crate) const PAGE_LINES: usize = 12;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Change {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<TextChange>,
    start: usize,
    before: Lines,
    after: Lines,
    page: usize,
    #[serde(skip)]
    typing: Option<String>,
}

impl Change {
    fn compact(&mut self) {
        if self.text.is_some()
            || self.before.is_empty()
            || self.after.is_empty()
            || self.before.id(0) != self.after.id(0)
        {
            return;
        }
        // Small rows retain the legacy representation. Large rows retain only
        // changed bytes, rather than charging their unchanged text to history.
        if self.before.byte_len().max(self.after.byte_len()) <= crate::documenttransfer::CHUNK_BYTES
        {
            return;
        }
        self.text = Some(TextChange::between_lines(&self.before, &self.after));
        self.before = Lines::default();
        self.after = Lines::default();
    }

    fn merge(&mut self, next: &Self) -> bool {
        if let Some(delta) = &self.text {
            let Some(merged) = delta.merged(&next.before, &next.after) else {
                return false;
            };
            self.text = Some(merged);
        } else {
            self.after = next.after.clone();
            self.compact();
        }
        true
    }

    fn apply(&self, lines: &mut Lines, undo: bool) -> bool {
        if let Some(delta) = &self.text {
            let Some(end) = delta
                .expected_rows(undo)
                .and_then(|rows| self.start.checked_add(rows))
            else {
                return false;
            };
            let Some(current) = lines.slice(self.start..end) else {
                return false;
            };
            let Some(replacement) = delta.apply_lines(&current, undo) else {
                return false;
            };
            return lines.splice(self.start..end, replacement);
        }
        let (expected, replacement) = if undo {
            (&self.after, &self.before)
        } else {
            (&self.before, &self.after)
        };
        let Some(end) = self.start.checked_add(expected.len()) else {
            return false;
        };
        if lines.slice(self.start..end).as_ref() != Some(expected) {
            return false;
        }
        lines.splice(self.start..end, replacement.clone())
    }

    fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.text.as_ref().map_or(0, TextChange::bytes))
            .saturating_add(self.before.retained_bytes())
            .saturating_add(self.after.retained_bytes())
            .saturating_add(self.typing.as_ref().map_or(0, String::capacity))
    }
}

impl Document {
    pub fn new(text: &str) -> Self {
        Self::from_lines(Lines::new(text, None))
    }

    fn from_lines(mut lines: Lines) -> Self {
        if lines.is_empty() {
            lines = Lines::new("", None);
        }
        Self {
            native: None,
            native_source: None,
            selection: None,
            preferred_column: None,
            ignore_selection_once: None,
            navigation: None,
            search: Search::default(),
            page: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            lines,
        }
    }

    pub fn text(&self) -> String {
        self.lines.text()
    }

    pub fn byte_len(&self) -> usize {
        self.lines.byte_len()
    }

    pub fn search_for(&mut self, query: &str) {
        self.selection = None;
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
            // The input selects its local segment; the draft tracks the full match.
            let end = (offset + self.search.query.len())
                .min(start + self.lines.text_at(line).unwrap().len());
            let to = text[start..end].chars().count();
            self.navigation = Some(Navigation {
                marked: None,
                line_id: self.lines.id(line).clone(),
                range: from..to,
                token: uuid::Uuid::new_v4().as_u128(),
            });
            self.selection = Some(Selection {
                anchor: position_at_byte(&text, offset),
                head: position_at_byte(&text, offset + self.search.query.len()),
            });
            if self.crosses_lines() {
                self.navigate_selection();
            }
        } else {
            self.selection = None;
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
        let before = self.lines.slice(first..last + 1).unwrap();
        let after = Lines::new(&edited, Some(before.id(0)));
        self.end_typing();
        self.lines.splice(first..last + 1, after.clone());
        self.record(Change {
            text: None,
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

    pub fn observe_selection(&mut self, id: &str, start: usize, end: usize, reversed: bool) {
        if self.ignore_selection_once.as_deref() == Some(id) {
            self.ignore_selection_once = None;
            return;
        }
        let Some(line) = self.lines.index_of(id) else {
            return;
        };
        self.native = None;
        self.native_source = None;
        let length = self.lines.text_at(line).unwrap().chars().count();
        self.preferred_column = None;
        let start = Position {
            line,
            column: start.min(length),
        };
        let end = Position {
            line,
            column: end.min(length),
        };
        self.selection = Some(if reversed {
            Selection {
                anchor: end,
                head: start,
            }
        } else {
            Selection {
                anchor: start,
                head: end,
            }
        });
    }

    pub fn extend_to(&mut self, id: &str, column: usize) {
        let Some(line) = self.lines.index_of(id) else {
            return;
        };
        self.native = None;
        self.native_source = None;
        let head = Position {
            line,
            column: column.min(self.lines.text_at(line).unwrap().chars().count()),
        };
        let anchor = self.selection.map_or(head, |selection| selection.anchor);
        self.selection = Some(Selection { anchor, head });
        self.preferred_column = None;
        self.end_typing();
        // The pointer already focused the destination; preserve its drag capture.
        self.page = head.line / PAGE_LINES;
        self.navigation = None;
    }

    pub fn crosses_lines(&self) -> bool {
        self.selection
            .is_some_and(|selection| selection.anchor.line != selection.head.line)
    }

    fn ordered_selection(&self) -> Option<(Position, Position)> {
        let selection = self.selection?;
        Some((
            selection.anchor.min(selection.head),
            selection.anchor.max(selection.head),
        ))
    }

    pub fn line_selection(&self, line: usize) -> Option<std::ops::Range<usize>> {
        let (start, end) = self.ordered_selection()?;
        if start == end || line < start.line || line > end.line {
            return None;
        }
        let from = if line == start.line { start.column } else { 0 };
        let to = if line == end.line {
            end.column
        } else {
            self.lines.get(line)?.text.chars().count()
        };
        Some(from..to)
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.ordered_selection()?;
        if start == end {
            return None;
        }
        let first = self.lines.text_at(start.line)?;
        let last = self.lines.text_at(end.line)?;
        let mut text = self.lines.slice(start.line..end.line + 1)?.text();
        text.truncate(text.len() - last.len() + scalar_byte(last, end.column));
        text.drain(..scalar_byte(first, start.column));
        Some(text)
    }

    fn navigate_selection(&mut self) {
        self.native = None;
        self.native_source = None;
        let Some(selection) = self.selection else {
            return;
        };
        self.page = selection.head.line / PAGE_LINES;
        self.navigation = Some(Navigation {
            marked: None,
            line_id: self.lines.id(selection.head.line).clone(),
            range: if selection.anchor.line == selection.head.line {
                selection.anchor.column.min(selection.head.column)
                    ..selection.anchor.column.max(selection.head.column)
            } else {
                selection.head.column..selection.head.column
            },
            token: uuid::Uuid::new_v4().as_u128(),
        });
    }

    pub fn select_all(&mut self) {
        self.preferred_column = None;
        let Some(last) = self.lines.len().checked_sub(1) else {
            return;
        };
        self.selection = Some(Selection {
            anchor: Position { line: 0, column: 0 },
            head: Position {
                line: last,
                column: self.lines.text_at(last).unwrap().chars().count(),
            },
        });
        self.end_typing();
        self.navigate_selection();
    }

    pub fn move_cursor(&mut self, movement: Movement, extend: bool) -> bool {
        let Some(mut selection) = self.selection else {
            return false;
        };
        self.end_typing();
        let mut head = selection.head;
        if matches!(movement, Movement::Up | Movement::Down) {
            head.column = *self.preferred_column.get_or_insert(head.column);
        } else {
            self.preferred_column = None;
        }
        if !extend && selection.anchor != selection.head {
            match movement {
                Movement::Left => head = selection.anchor.min(selection.head),
                Movement::Right => head = selection.anchor.max(selection.head),
                _ => {}
            }
            if matches!(movement, Movement::Left | Movement::Right) {
                self.selection = Some(Selection { anchor: head, head });
                self.navigate_selection();
                return true;
            }
        }
        let length = self.lines.text_at(head.line).unwrap().chars().count();
        match movement {
            Movement::Up => head.line = head.line.saturating_sub(1),
            Movement::Down => head.line = (head.line + 1).min(self.lines.len() - 1),
            Movement::Left if head.column > 0 => head.column -= 1,
            Movement::Left if head.line > 0 => {
                head.line -= 1;
                head.column = self.lines.text_at(head.line).unwrap().chars().count();
            }
            Movement::Right if head.column < length => head.column += 1,
            Movement::Right if head.line + 1 < self.lines.len() => {
                head.line += 1;
                head.column = 0;
            }
            Movement::Start => head.column = 0,
            Movement::End => head.column = length,
            _ => {}
        }
        head.column = head
            .column
            .min(self.lines.text_at(head.line).unwrap().chars().count());
        selection.head = head;
        if !extend {
            selection.anchor = head;
        }
        self.selection = Some(selection);
        self.navigate_selection();
        true
    }

    /// Replaces the selected document range as one undoable splice.
    pub fn replace_selection(&mut self, replacement: &str) -> bool {
        let Some((start, end)) = self.ordered_selection() else {
            return false;
        };
        let Some(first) = self.lines.get(start.line) else {
            return false;
        };
        let Some(last) = self.lines.get(end.line) else {
            return false;
        };
        let edited = format!(
            "{}{}{}",
            &first.text[..scalar_byte(&first.text, start.column)],
            replacement,
            &last.text[scalar_byte(&last.text, end.column)..]
        );
        let before = self.lines.slice(start.line..end.line + 1).unwrap();
        let after = Lines::new(&edited, Some(before.id(0)));
        if before == after {
            return false;
        }
        self.end_typing();
        self.lines.splice(start.line..end.line + 1, after.clone());
        self.record(Change {
            text: None,
            start: start.line,
            before,
            after,
            page: self.page,
            typing: None,
        });
        let added_lines = replacement.bytes().filter(|byte| *byte == b'\n').count();
        let head = Position {
            line: start.line + added_lines,
            column: if added_lines == 0 {
                start.column + replacement.chars().count()
            } else {
                replacement
                    .rsplit('\n')
                    .next()
                    .unwrap_or_default()
                    .chars()
                    .count()
            },
        };
        self.selection = Some(Selection { anchor: head, head });
        self.navigate_selection();
        true
    }

    pub fn set(&mut self, id: &str, text: &str) {
        if self
            .lines
            .index_of(id)
            .and_then(|row| self.lines.text_at(row))
            == Some(text)
        {
            return;
        }
        // Native IME/paste edits arrive through the focused line's input.
        if self.crosses_lines()
            && let Some(selection) = self.selection
            && self.lines.id(selection.head.line) == id
        {
            let old = &self.lines.text_at(selection.head.line).unwrap();
            let prefix = old
                .chars()
                .zip(text.chars())
                .take_while(|(a, b)| a == b)
                .count();
            let suffix = old
                .chars()
                .rev()
                .zip(text.chars().rev())
                .take(
                    old.chars()
                        .count()
                        .saturating_sub(prefix)
                        .min(text.chars().count().saturating_sub(prefix)),
                )
                .take_while(|(a, b)| a == b)
                .count();
            let insertion =
                &text[scalar_byte(text, prefix)..scalar_byte(text, text.chars().count() - suffix)];
            if self.replace_selection(insertion) {
                self.ignore_selection_once = Some(id.into());
            }
            return;
        }
        if let Some(index) = self.lines.index_of(id) {
            if self.lines.text_at(index).unwrap() == text {
                return;
            }
            let before = self.lines.slice(index..index + 1).unwrap();
            let after = Lines::new(text, Some(id.into()));
            let count = after.len();
            self.lines.splice(index..index + 1, after.clone());
            self.record(Change {
                text: None,
                start: index,
                before,
                after,
                page: self.page,
                typing: (count == 1).then(|| id.to_owned()),
            });
        }
    }

    /// Applies explicit native edits and rebases composition when document rows merge.
    pub fn native_edit(&mut self, source: &str, edit: NativeEdit<'_>) -> Option<NativeState> {
        let owner = self
            .native
            .as_ref()
            .filter(|state| {
                state.line_id == source || self.native_source.as_deref() == Some(source)
            })
            .map_or_else(|| source.to_owned(), |state| state.line_id.clone());
        let line = self.lines.index_of(&owner)?;
        if matches!(edit, NativeEdit::Unmark) {
            self.end_typing();
            let native = self.native.as_mut()?;
            native.marked = None;
            if let Some(navigation) = &mut self.navigation {
                navigation.marked = None;
            }
            return Some(native.clone());
        }
        let NativeEdit::Replace {
            range,
            text,
            composing,
            selected,
        } = edit
        else {
            return None;
        };
        let composing = composing && !text.contains('\n');
        let cross = self.crosses_lines()
            && self
                .selection
                .is_some_and(|selection| selection.head.line == line);
        let insertion = if cross {
            self.ordered_selection()?.0
        } else {
            Position {
                line,
                column: range
                    .start
                    .min(self.lines.text_at(line).unwrap().chars().count()),
            }
        };
        let prior_mark = self
            .native
            .as_ref()
            .is_some_and(|state| state.marked.is_some());
        if !prior_mark {
            self.end_typing();
        }
        if cross {
            self.replace_selection(text);
        } else {
            let old = &self.lines.text_at(line).unwrap();
            let end = range.end.max(insertion.column).min(old.chars().count());
            let value = format!(
                "{}{}{}",
                &old[..scalar_byte(old, insertion.column)],
                text,
                &old[scalar_byte(old, end)..]
            );
            self.set(&owner, &value);
        }
        let added = text.bytes().filter(|byte| *byte == b'\n').count();
        let target = insertion.line + added;
        let row = self.lines.get(target)?;
        let end = if added == 0 {
            insertion.column + text.chars().count()
        } else {
            text.rsplit('\n').next().unwrap_or_default().chars().count()
        };
        let selected = if composing {
            selected
                .map(|selection| {
                    insertion.column + selection.start.min(text.chars().count())
                        ..insertion.column
                            + selection.end.max(selection.start).min(text.chars().count())
                })
                .unwrap_or(end..end)
        } else {
            end..end
        };
        let result = NativeState {
            line_id: row.id.clone(),
            text: row.text.clone(),
            selected,
            marked: (composing && !text.is_empty()).then_some(insertion.column..end),
            token: uuid::Uuid::new_v4().as_u128(),
        };
        if cross
            && composing
            && let Some(change) = self.undo.last_mut()
        {
            change.typing = Some(result.line_id.clone());
        }
        if result.marked.is_none() {
            self.end_typing();
        }
        self.page = target / PAGE_LINES;
        self.selection = Some(Selection {
            anchor: Position {
                line: target,
                column: result.selected.start,
            },
            head: Position {
                line: target,
                column: result.selected.end,
            },
        });
        self.ignore_selection_once = None;
        self.navigation = Some(Navigation {
            marked: result.marked.clone(),
            line_id: result.line_id.clone(),
            range: result.selected.clone(),
            token: result.token,
        });
        self.native_source = (source != result.line_id).then(|| source.to_owned());
        self.native = Some(result.clone());
        Some(result)
    }

    /// Ends a real row edit while preserving a composition moving to another row.
    pub fn native_edit_end(&mut self, source: &str) {
        if self
            .native
            .as_ref()
            .is_some_and(|state| state.line_id != source)
        {
            return;
        }
        self.native = None;
        self.native_source = None;
        self.navigation = None;
        self.end_typing();
    }

    pub fn insert_after(&mut self, id: &str) {
        if let Some(index) = self.lines.index_of(id) {
            self.end_typing();
            let after = Lines::new("", None);
            self.lines.splice(index + 1..index + 1, after.clone());
            self.record(Change {
                text: None,
                start: index + 1,
                before: Lines::default(),
                after,
                page: self.page,
                typing: None,
            });
        }
    }

    pub fn remove(&mut self, id: &str) {
        let Some(index) = self.lines.index_of(id) else {
            return;
        };
        self.end_typing();
        if self.lines.len() == 1 {
            self.set(id, "");
            self.end_typing();
        } else {
            let before = self.lines.slice(index..index + 1).unwrap();
            self.lines.splice(index..index + 1, Lines::default());
            self.record(Change {
                text: None,
                start: index,
                before,
                after: Lines::default(),
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
        self.native = None;
        self.native_source = None;
        self.end_typing();
        self.page = (line - 1) / PAGE_LINES;
        self.preferred_column = None;
        let head = Position {
            line: line - 1,
            column: 0,
        };
        self.selection = Some(Selection { anchor: head, head });
        self.navigation = Some(Navigation {
            marked: None,
            line_id: self.lines.id(line - 1).clone(),
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
        self.native = None;
        self.native_source = None;
        self.selection = None;
        self.preferred_column = None;
        self.ignore_selection_once = None;
        self.navigation = None;
        self.end_typing();
        if let Some(mut change) = self.undo.pop() {
            self.search.offset = None;
            if !change.apply(&mut self.lines, true) {
                self.undo.clear();
                self.redo.clear();
                return;
            }
            self.page = change.page.min(self.pages() - 1);
            change.typing = None;
            self.redo.push(change);
        }
    }

    pub fn redo(&mut self) {
        self.native = None;
        self.native_source = None;
        self.selection = None;
        self.preferred_column = None;
        self.ignore_selection_once = None;
        self.navigation = None;
        self.end_typing();
        if let Some(change) = self.redo.pop() {
            self.search.offset = None;
            if !change.apply(&mut self.lines, false) {
                self.undo.clear();
                self.redo.clear();
                return;
            }
            self.page = (change.start / PAGE_LINES).min(self.pages() - 1);
            self.undo.push(change);
        }
    }

    pub fn end_typing(&mut self) {
        if let Some(change) = self.undo.last_mut() {
            change.typing = None;
        }
    }

    fn record(&mut self, mut change: Change) {
        self.native = None;
        self.native_source = None;
        self.selection = None;
        self.preferred_column = None;
        self.ignore_selection_once = None;
        self.navigation = None;
        self.search.offset = None;
        self.redo.clear();
        let merged = self.undo.last_mut().is_some_and(|previous| {
            change.typing.is_some()
                && previous.typing == change.typing
                && previous.start == change.start
                && previous.merge(&change)
        });
        if !merged {
            change.compact();
            self.undo.push(change);
        }
        self.trim_history();
    }

    fn trim_history(&mut self) {
        // Keep the nearest undo steps first, then the nearest redo steps.
        // A single pass also bounds recovery work for older, oversized journals.
        let mut bytes = 0usize;
        let mut changes = 0usize;
        let mut exhausted = false;
        for history in [&mut self.undo, &mut self.redo] {
            let mut keep = 0;
            for change in history.iter().rev() {
                let next_bytes = bytes.saturating_add(change.bytes());
                if exhausted || changes == HISTORY_CHANGES || next_bytes > HISTORY_BYTES {
                    exhausted = true;
                    break;
                }
                bytes = next_bytes;
                changes += 1;
                keep += 1;
            }
            history.drain(..history.len() - keep);
            if history.capacity() > HISTORY_CHANGES * 2 {
                history.shrink_to_fit();
            }
        }
    }
}

fn scalar_byte(text: &str, column: usize) -> usize {
    text.char_indices()
        .nth(column)
        .map_or(text.len(), |(byte, _)| byte)
}

fn position_at_byte(text: &str, offset: usize) -> Position {
    let prefix = &text[..offset];
    Position {
        line: prefix.bytes().filter(|byte| *byte == b'\n').count(),
        column: prefix
            .rsplit('\n')
            .next()
            .unwrap_or_default()
            .chars()
            .count(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn splitting_and_rejoining_large_rows_restores_compressed_line_identities() {
        let row = "x".repeat(HISTORY_BYTES * 2);
        let original = format!("{row}\ntrailer");
        let mut document = Document::new(&original);
        let first = document.lines.id(0);
        let trailer = document.lines.id(1);
        let middle = row.len() / 2;
        let expanded = format!(
            "{}{}{}",
            &row[..middle],
            "\n".repeat(10_000),
            &row[middle..]
        );
        document.set(&first, &expanded);
        document.end_typing();
        let middle_id = document.lines.id(5000);
        let last_id = document.lines.id(10_000);
        assert!(document.can_undo());
        assert!(document.undo.iter().map(Change::bytes).sum::<usize>() < 32 * 1024);
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        recovered.undo();
        assert!(recovered.text() == original);
        assert_eq!(recovered.lines.id(0), first);
        assert_eq!(recovered.lines.id(1), trailer);
        recovered.redo();
        assert_eq!(recovered.lines.len(), 10_002);
        assert_eq!(recovered.lines.id(5000), middle_id);
        assert_eq!(recovered.lines.id(10_000), last_id);
        assert_eq!(recovered.lines.id(10_001), trailer);
        recovered.selection = Some(Selection {
            anchor: Position {
                line: 0,
                column: middle,
            },
            head: Position {
                line: 10_000,
                column: 0,
            },
        });
        assert!(recovered.replace_selection(""));
        assert!(recovered.text() == original);
        assert!(recovered.undo.iter().map(Change::bytes).sum::<usize>() < 64 * 1024);
        recovered.undo();
        assert_eq!(recovered.lines.id(5000), middle_id);
        assert_eq!(recovered.lines.id(10_000), last_id);
        recovered.undo();
        assert!(recovered.text() == original);
        recovered.redo();
        recovered.redo();
        assert!(recovered.text() == original);
        assert_eq!(recovered.lines.id(1), trailer);
    }

    #[test]
    fn small_edits_to_a_file_sized_line_keep_compact_history_and_recover_undo() {
        let original = "x".repeat(crate::documenttransfer::DOCUMENT_BYTES - 16);
        let mut document = Document::new(&original);
        let id = document.lines.id(0);
        let mut edited = original.clone();
        let middle = edited.len() / 2;
        edited.replace_range(middle..middle + 1, "🦀");
        document.set(&id, &edited);
        assert!(document.can_undo());
        assert!(
            document.undo.iter().map(Change::bytes).sum::<usize>() < 4096,
            "An unchanged 32 MiB prefix/suffix must not be retained by undo"
        );
        assert!(serde_json::to_string(&document.undo).unwrap().len() < 4096);
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        recovered.undo();
        assert!(
            recovered.text() == original,
            "Recovered undo must restore every long-line byte"
        );
        assert_eq!(recovered.lines.id(0), id);
        recovered.redo();
        assert!(
            recovered.text() == edited,
            "Recovered redo must restore every edited byte"
        );
        assert_eq!(recovered.lines.id(0), id);
    }

    #[test]
    fn long_line_typing_groups_and_composition_preserve_scope_across_recovery() {
        let original = "aé🦀".repeat(16_000);
        let mut document = Document::new(&original);
        let id = document.lines.id(0);
        document.set(&id, &format!("{original}に"));
        document.set(&id, &format!("{original}日本"));
        assert_eq!(document.undo.len(), 1);
        document.end_typing();
        document.set(&id, &format!("A{original}日本"));
        assert_eq!(document.undo.len(), 2);
        assert!(document.undo.iter().map(Change::bytes).sum::<usize>() < 4096);
        let encoded = serde_json::to_string(&document).unwrap();
        let mut recovered: Document = serde_json::from_str(&encoded).unwrap();
        recovered.undo();
        assert!(recovered.text() == format!("{original}日本"));
        recovered.undo();
        assert!(recovered.text() == original);
        recovered.redo();
        recovered.redo();
        assert!(recovered.text() == format!("A{original}日本"));

        let mut stale: Document = serde_json::from_str(&encoded).unwrap();
        stale.undo();
        let mut changed = stale.text();
        changed.replace_range(0..1, "z"); // Outside the next inverse's changed range.
        stale
            .lines
            .splice(0..1, Lines::new(&changed, Some(id.clone())));
        stale.undo();
        assert!(
            stale.text() == changed,
            "Stale history must not alter an unrelated revision"
        );
        assert!(!stale.can_undo() && !stale.can_redo());

        let mut composing = Document::new(&original);
        let source = composing.lines.id(0);
        composing
            .native_edit(
                &source,
                NativeEdit::Replace {
                    range: 100..100,
                    text: "に",
                    composing: true,
                    selected: None,
                },
            )
            .unwrap();
        composing
            .native_edit(
                &source,
                NativeEdit::Replace {
                    range: 100..101,
                    text: "日本",
                    composing: true,
                    selected: None,
                },
            )
            .unwrap();
        composing.native_edit(&source, NativeEdit::Unmark).unwrap();
        assert_eq!(composing.undo.len(), 1);
        composing.undo();
        assert!(
            composing.text() == original,
            "A long-line composition must undo as one change"
        );
    }

    #[test]
    fn cross_line_selection_copies_unicode_and_pastes_as_one_undoable_change() {
        let mut document = Document::new("a🦀é\nnext\ntail");
        let before = document.lines.clone();
        let id = document.lines.get(0).unwrap().id.clone();
        document.observe_selection(&id, 1, 1, false);
        document.move_cursor(Movement::Down, true);
        document.move_cursor(Movement::Right, true);
        assert_eq!(document.selected_text().as_deref(), Some("🦀é\nne"));
        assert_eq!(document.line_selection(0), Some(1..3));
        assert_eq!(document.line_selection(1), Some(0..2));
        assert!(document.replace_selection("X\nY"));
        assert_eq!(document.text(), "aX\nYxt\ntail");
        assert_eq!(
            document.selection.unwrap().head,
            Position { line: 1, column: 1 }
        );
        assert_eq!(document.lines.get(2).unwrap().id, before.get(2).unwrap().id);
        document.undo();
        assert_eq!(document.lines, before);
        document.redo();
        assert_eq!(document.text(), "aX\nYxt\ntail");
    }

    #[test]
    fn reverse_selection_crosses_pages_and_vertical_movement_keeps_the_column() {
        let text = format!("longer\nx\n{}last", "row\n".repeat(PAGE_LINES));
        let mut document = Document::new(&text);
        let first = document.lines.get(0).unwrap().id.clone();
        document.observe_selection(&first, 5, 5, false);
        document.move_cursor(Movement::Down, true);
        assert_eq!(document.selection.unwrap().head.column, 1);
        document.move_cursor(Movement::Down, true);
        assert_eq!(document.selection.unwrap().head.column, 3);
        document.move_cursor(Movement::Up, true);
        document.move_cursor(Movement::Up, true);
        assert_eq!(document.selection.unwrap().head.column, 5);
        let last = document.lines.last().unwrap().id.clone();
        document.observe_selection(&last, 2, 2, false);
        for _ in 0..PAGE_LINES + 2 {
            document.move_cursor(Movement::Up, true);
        }
        assert_eq!(document.page, 0);
        assert!(document.crosses_lines());
        assert!(document.selected_text().unwrap().starts_with("nger\nx\n"));
        document.move_cursor(Movement::Left, false);
        assert!(document.selected_text().is_none());
        assert_eq!(
            document.selection.unwrap().head,
            Position { line: 0, column: 2 }
        );
    }

    #[test]
    fn select_all_retains_final_newlines_and_selection_is_not_persisted() {
        let mut document = Document::new("first\nsecond\n");
        let before = document.lines.clone();
        document.select_all();
        assert_eq!(document.selected_text().as_deref(), Some("first\nsecond\n"));
        let restored: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(restored.selection.is_none());
        assert!(document.replace_selection(""));
        assert_eq!(document.text(), "");
        assert_eq!(document.lines.len(), 1);
        document.undo();
        assert_eq!(document.lines, before);
    }

    #[test]
    fn pointer_extension_retains_the_anchor_across_click_drag_and_release() {
        let mut document = Document::new("first\nsecond\nlast");
        let first = document.lines.get(0).unwrap().id.clone();
        let last = document.lines.get(2).unwrap().id.clone();
        document.observe_selection(&first, 2, 2, false);
        document.extend_to(&last, 1);
        document.extend_to(&last, 3);
        document.extend_to(&last, 3);
        assert_eq!(
            document.selected_text().as_deref(),
            Some("rst\nsecond\nlas")
        );
        document.observe_selection(&last, 1, 1, false);
        assert!(document.selected_text().is_none());
    }

    #[test]
    fn native_cross_line_replacement_preserves_the_new_cursor_and_ignores_unchanged_edits() {
        let mut document = Document::new("abc\ndef");
        let head_id = document.lines.get(0).unwrap().id.clone();
        let last_id = document.lines.get(1).unwrap().id.clone();
        document.observe_selection(&last_id, 2, 2, false);
        document.move_cursor(Movement::Up, true);
        document.move_cursor(Movement::Left, true);
        document.set(&head_id, "abc");
        assert!(document.crosses_lines());
        // A native text/IME insertion is delivered to the currently focused line.
        document.set(&head_id, "aX\nYbc");
        assert_eq!(document.text(), "aX\nYf");
        assert_eq!(
            document.selection.unwrap().head,
            Position { line: 1, column: 1 }
        );
        document.observe_selection(&head_id, 4, 4, false);
        assert_eq!(
            document.selection.unwrap().head,
            Position { line: 1, column: 1 }
        );
        document.undo();
        assert_eq!(document.text(), "abc\ndef");
    }

    #[test]
    fn multi_line_search_selects_the_entire_match_for_copy_and_deletion() {
        let mut document = Document::new("first 🦀\nnext é\ntail");
        document.search_for("🦀\nnext");
        document.find(false);
        assert_eq!(document.selected_text().as_deref(), Some("🦀\nnext"));
        assert!(document.replace_selection(""));
        assert_eq!(document.text(), "first  é\ntail");
        document.undo();
        assert_eq!(document.text(), "first 🦀\nnext é\ntail");
    }

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
        let id = document.lines.get(0).unwrap().id.clone();
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
        assert_eq!(navigation.line_id, document.lines.get(0).unwrap().id);
        assert_eq!(navigation.range, 1..3);
        let token = navigation.token;
        document.find(false);
        assert_ne!(document.navigation.as_ref().unwrap().token, token);
        assert!(document.go_to_line(2));
        assert_eq!(document.navigation.as_ref().unwrap().range, 0..0);
        assert_eq!(
            document.navigation.as_ref().unwrap().line_id,
            document.lines.get(1).unwrap().id
        );
        let restored: super::Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert!(restored.navigation.is_none());
        assert_eq!(restored.text(), "a🦀éz\nsecond line");
        let id = document.lines.get(1).unwrap().id.clone();
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
        let id = document.lines.get(0).unwrap().id.clone();
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
        let first = document.lines.get(0).unwrap().id.clone();
        let last = document.lines.get(1).unwrap().id.clone();
        document.insert_after(&first);
        assert_eq!(document.lines.get(2).unwrap().id, last);
        let inserted = document.lines.get(1).unwrap().id.clone();
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
        assert_eq!(document.lines.get(3).unwrap().id, last);
    }

    #[test]
    fn undo_groups_typing_and_restores_pastes_and_line_identity() {
        let mut document = Document::new("first\nlast\n");
        let first = document.lines.get(0).unwrap().id.clone();
        let last = document.lines.get(1).unwrap().id.clone();
        document.set(&first, "f");
        document.set(&first, "foo 🦀");
        document.undo();
        assert_eq!(document.text(), "first\nlast\n");
        assert!(!document.can_undo());
        document.redo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
        document.set(&first, "one\ntwo\nthree");
        let inserted = document.lines.get(1).unwrap().id.clone();
        document.undo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
        document.redo();
        assert_eq!(document.lines.get(1).unwrap().id, inserted);
        assert_eq!(document.lines.get(3).unwrap().id, last);
        document.remove(&last);
        document.undo();
        assert_eq!(document.lines.get(3).unwrap().id, last);
        document.undo();
        document.set(&first, "alternative");
        assert!(!document.can_redo());
        document.undo();
        assert_eq!(document.text(), "foo 🦀\nlast\n");
    }

    #[test]
    fn history_survives_recovery_without_merging_a_new_edit() {
        let mut document = Document::new("original");
        let id = document.lines.get(0).unwrap().id.clone();
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
    fn newline_heavy_paste_keeps_bounded_history_without_per_row_records() {
        let mut document = Document::new("original");
        let id = document.lines.get(0).unwrap().id.clone();
        // Blank rows retain chunks, rather than one allocated ID per newline.
        let text = "\n".repeat(64 * 1024);
        document.set(&id, &text);
        assert_eq!(document.text(), text);
        assert!(document.can_undo());
        assert!(document.undo.iter().map(Change::bytes).sum::<usize>() < text.len() * 3);
        assert!(serde_json::to_string(&document).unwrap().len() < text.len() * 7);
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert_eq!(recovered.text(), text);
        recovered.undo();
        assert_eq!(recovered.text(), "original");
        recovered.redo();
        assert_eq!(recovered.text(), text);
        recovered.set(&id, "edited");
        recovered.undo();
        assert_eq!(recovered.text(), text);
        recovered.redo();
        assert!(recovered.text().starts_with("edited\n"));
    }

    #[test]
    fn chunked_document_handles_the_file_limit_without_per_line_allocations() {
        let text = "\n".repeat(crate::documenttransfer::DOCUMENT_BYTES);
        let mut document = Document::new(&text);
        assert_eq!(document.lines.len(), text.len() + 1);
        assert_eq!(document.byte_len(), text.len());
        assert!(document.lines.retained_bytes() < text.len() + 1024 * 1024);
        let last = document.lines.id(text.len());
        assert!(document.go_to_line(text.len() + 1));
        assert_eq!(document.navigation.as_ref().unwrap().line_id, last);
        // Skip directly to the last page, without materializing hidden rows.
        assert_eq!(document.lines.iter().nth(text.len()).unwrap().id, last);
        document.select_all();
        assert!(
            document.selected_text().as_deref() == Some(text.as_str()),
            "Select all must preserve every newline"
        );
        document.observe_selection(&last, 0, 0, false);
        document.set(&last, "tail 🦀");
        assert_eq!(document.byte_len(), text.len() + "tail 🦀".len());
        document.undo();
        assert_eq!(document.byte_len(), text.len());
        assert!(
            document.text() == text,
            "Undo must restore the complete large document"
        );
        assert_eq!(document.lines.id(text.len()), last);
    }

    #[test]
    fn chunked_document_recovery_undo_crosses_text_chunk_boundaries() {
        let mut document = Document::new(&"\n".repeat(70 * 1024));
        let original = document.lines.clone();
        let start = 32 * 1024 - 2;
        let first = document.lines.id(start);
        document.selection = Some(Selection {
            anchor: Position {
                line: start,
                column: 0,
            },
            head: Position {
                line: start + 4,
                column: 0,
            },
        });
        assert!(document.replace_selection("joined 🦀\nnext"));
        let edited = document.lines.clone();
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert_eq!(recovered.lines.id(start), first);
        recovered.undo();
        assert_eq!(recovered.lines, original);
        recovered.redo();
        assert_eq!(recovered.lines, edited);
    }

    #[test]
    fn chunked_document_recovers_legacy_undo_records_and_line_identities() {
        let mut document: Document = serde_json::from_value(serde_json::json!({
            "lines":[{"id":"old","text":"edited"}],
            "undo":[{"start":0,"page":0,"before":[{"id":"old","text":"original"}],"after":[{"id":"old","text":"edited"}]}]
        })).unwrap();
        document.undo();
        assert_eq!(document.text(), "original");
        assert_eq!(document.lines.id(0), "old");
        document.redo();
        assert_eq!(document.text(), "edited");
    }

    #[test]
    fn recovery_trims_legacy_history_across_undo_and_redo_without_editing_text() {
        let mut document = Document::new("original");
        let id = document.lines.get(0).unwrap().id.clone();
        document.set(&id, &"x".repeat(200 * 1024));
        let change = document.undo[0].clone();
        document.undo = vec![change.clone(); 12];
        document.redo = vec![change; 40];
        let encoded = serde_json::to_string(&document).unwrap();
        let recovered: Document = serde_json::from_str(&encoded).unwrap();
        assert_eq!(recovered.text(), document.text());
        assert_eq!(recovered.undo.len(), 12);
        assert!(recovered.redo.len() < 40);
        assert!(
            recovered
                .undo
                .iter()
                .chain(&recovered.redo)
                .map(Change::bytes)
                .sum::<usize>()
                <= HISTORY_BYTES
        );
        assert!(recovered.undo.len() + recovered.redo.len() <= HISTORY_CHANGES);

        let empty: Document = serde_json::from_value(serde_json::json!({
            "lines": [], "page": usize::MAX
        }))
        .unwrap();
        assert_eq!(empty.text(), "");
        assert_eq!(empty.page, 0);
        assert_eq!(empty.lines.len(), 1);
    }

    #[test]
    fn history_counts_retained_chunks_and_metadata() {
        let change = Change {
            text: None,
            start: 0,
            before: Lines::new(&"x".repeat(4096), None),
            after: Lines::default(),
            page: 0,
            typing: None,
        };
        assert!(change.bytes() >= 4096);
        assert!(change.bytes() < 8192);
    }

    #[test]
    fn recovery_preserves_the_nearest_undo_and_redo_steps() {
        let mut document = Document::new("original");
        let id = document.lines.get(0).unwrap().id.clone();
        for text in ["first", "second", "third"] {
            document.end_typing();
            document.set(&id, text);
        }
        document.undo();
        document.undo();
        let mut recovered: Document =
            serde_json::from_str(&serde_json::to_string(&document).unwrap()).unwrap();
        assert_eq!(recovered.text(), "first");
        recovered.redo();
        assert_eq!(recovered.text(), "second");
        recovered.redo();
        assert_eq!(recovered.text(), "third");
        recovered.undo();
        assert_eq!(recovered.text(), "second");
    }

    #[test]
    fn navigation_and_history_are_bounded_and_corrupt_history_does_not_change_text() {
        let mut document = Document::new(&vec!["line"; 25].join("\n"));
        assert_eq!(document.pages(), 3);
        assert!(!document.go_to_line(0));
        assert!(!document.go_to_line(26));
        assert!(document.go_to_line(25));
        assert_eq!(document.page, 2);
        let id = document.lines.get(0).unwrap().id.clone();
        for index in 0..HISTORY_CHANGES + 1 {
            document.end_typing();
            document.set(&id, &index.to_string());
        }
        assert_eq!(document.undo.len(), HISTORY_CHANGES);
        let mut large = Document::new("small");
        let id = large.lines.get(0).unwrap().id.clone();
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
