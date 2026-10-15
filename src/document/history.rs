//! Compact, revision-checked history for edits across large document row spans.

// Rust guideline compliant 2026-02-21
use super::lines::{Identities, Lines};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct TextChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    before_ids: Option<Identities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after_ids: Option<Identities>,
    pub line_id: String,
    offset: usize,
    before: String,
    after: String,
    before_len: usize,
    after_len: usize,
    before_hash: String,
    after_hash: String,
}

impl std::fmt::Debug for TextChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TextChange")
            .field("offset", &self.offset)
            .field("removed_bytes", &self.before.len())
            .field("inserted_bytes", &self.after.len())
            .finish_non_exhaustive()
    }
}

impl TextChange {
    pub fn between_lines(before: &Lines, after: &Lines) -> Self {
        let mut change = Self::between(before.id(0), &block_text(before), &block_text(after));
        change.before_ids = Some(before.identities());
        change.after_ids = Some(after.identities());
        change
    }

    pub fn expected_rows(&self, undo: bool) -> Option<usize> {
        let ids = if undo {
            &self.after_ids
        } else {
            &self.before_ids
        };
        ids.as_ref().map_or(Some(1), Identities::rows)
    }

    fn matches(&self, lines: &Lines, undo: bool) -> bool {
        let ids = if undo {
            &self.after_ids
        } else {
            &self.before_ids
        };
        ids.as_ref().map_or_else(
            || lines.len() == 1 && lines.id(0) == self.line_id,
            |ids| ids.matches(lines),
        )
    }

    pub fn apply_lines(&self, lines: &Lines, undo: bool) -> Option<Lines> {
        if !self.matches(lines, undo) {
            return None;
        }
        let text = self.apply(&block_text(lines), undo)?;
        let ids = if undo {
            &self.before_ids
        } else {
            &self.after_ids
        };
        if let Some(ids) = ids {
            Lines::with_identities(&text, ids)
        } else if !text.contains('\n') {
            Some(Lines::new(&text, Some(self.line_id.clone())))
        } else {
            None
        }
    }

    pub fn merged(&self, before: &Lines, after: &Lines) -> Option<Self> {
        if !self.matches(before, true) || after.is_empty() || after.id(0) != self.line_id {
            return None;
        }
        let original = self.apply(&block_text(before), true)?;
        let mut change = Self::between(self.line_id.clone(), &original, &block_text(after));
        change.before_ids = self.before_ids.clone();
        change.after_ids = Some(after.identities());
        Some(change)
    }

    pub fn between(line_id: String, before: &str, after: &str) -> Self {
        let mut offset = before
            .bytes()
            .zip(after.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(offset) || !after.is_char_boundary(offset) {
            offset -= 1;
        }
        let mut suffix = before[offset..]
            .bytes()
            .rev()
            .zip(after[offset..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(before.len() - suffix)
            || !after.is_char_boundary(after.len() - suffix)
        {
            suffix -= 1;
        }
        Self {
            before_ids: None,
            after_ids: None,
            line_id,
            offset,
            before: before[offset..before.len() - suffix].into(),
            after: after[offset..after.len() - suffix].into(),
            before_len: before.len(),
            after_len: after.len(),
            before_hash: crate::documenttransfer::digest(before.as_bytes()),
            after_hash: crate::documenttransfer::digest(after.as_bytes()),
        }
    }

    pub fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.before_ids.as_ref().map_or(0, Identities::bytes))
            .saturating_add(self.after_ids.as_ref().map_or(0, Identities::bytes))
            .saturating_add(self.line_id.capacity())
            .saturating_add(self.before.capacity())
            .saturating_add(self.after.capacity())
            .saturating_add(self.before_hash.capacity())
            .saturating_add(self.after_hash.capacity())
    }

    pub fn apply(&self, line: &str, undo: bool) -> Option<String> {
        let (expected, replacement, length, target_len, hash, target_hash) = if undo {
            (
                &self.after,
                &self.before,
                self.after_len,
                self.before_len,
                &self.after_hash,
                &self.before_hash,
            )
        } else {
            (
                &self.before,
                &self.after,
                self.before_len,
                self.after_len,
                &self.before_hash,
                &self.after_hash,
            )
        };
        if line.len() != length || crate::documenttransfer::digest(line.as_bytes()) != *hash {
            return None;
        }
        let end = self.offset.checked_add(expected.len())?;
        if line.get(self.offset..end)? != expected {
            return None;
        }
        let bytes = line
            .len()
            .checked_sub(expected.len())?
            .checked_add(replacement.len())?;
        if bytes != target_len {
            return None;
        }
        let mut result = String::with_capacity(bytes);
        result.push_str(&line[..self.offset]);
        result.push_str(replacement);
        result.push_str(&line[end..]);
        (crate::documenttransfer::digest(result.as_bytes()) == *target_hash).then_some(result)
    }
}

fn block_text(lines: &Lines) -> Cow<'_, str> {
    if lines.len() == 1 {
        Cow::Borrowed(lines.text_at(0).unwrap())
    } else {
        Cow::Owned(lines.text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_ranges_preserve_utf8_boundaries_and_refuse_corrupt_or_stale_history() {
        for (before, after) in [
            ("αé🦀end", "αΩ🦀end"),
            ("secret", ""),
            ("", "secret"),
            ("same", "same"),
        ] {
            let change = TextChange::between("line".into(), before, after);
            assert_eq!(change.apply(after, true).as_deref(), Some(before));
            assert_eq!(change.apply(before, false).as_deref(), Some(after));
            assert!(change.apply("unrelated", true).is_none());
            assert!(!format!("{change:?}").contains("secret"));
            let mut corrupt = change.clone();
            corrupt.offset = usize::MAX;
            assert!(corrupt.apply(after, true).is_none());
            corrupt = change.clone();
            corrupt.before_len = usize::MAX;
            assert!(corrupt.apply(after, true).is_none());
            corrupt = change;
            corrupt.before_hash = "0".repeat(64);
            assert!(corrupt.apply(after, true).is_none());
        }
    }
}
