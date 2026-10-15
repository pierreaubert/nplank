//! Chunked document rows with lazy identities and shared undo snapshots.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct as _};
use std::{ops::Range, sync::Arc};

/// A visible row, materialized only when requested by the editor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Line {
    pub id: String,
    pub text: String,
}

// Keep scans and ordinary undo retention bounded without indexing every newline.
const CHUNK_BYTES: usize = 32 * 1024;

/// Compressed row identities without retaining their source text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Identities(Vec<Identity>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    seed: String,
    first: usize,
    rows: usize,
    first_id: Option<String>,
}

impl Identities {
    pub fn rows(&self) -> Option<usize> {
        self.0.iter().try_fold(0usize, |total, piece| {
            if piece.rows == 0 || piece.first.checked_add(piece.rows).is_none() {
                return None;
            }
            total.checked_add(piece.rows)
        })
    }

    pub fn bytes(&self) -> usize {
        self.0.iter().fold(
            self.0.capacity() * std::mem::size_of::<Identity>(),
            |bytes, piece| {
                bytes
                    .saturating_add(piece.seed.capacity())
                    .saturating_add(piece.first_id.as_ref().map_or(0, String::capacity))
            },
        )
    }

    pub fn matches(&self, lines: &Lines) -> bool {
        self.rows() == Some(lines.len()) && *self == lines.identities()
    }
}

#[derive(Clone)]
struct Piece {
    source: Arc<str>,
    bytes: Range<usize>,
    seed: String,
    first: usize,
    first_id: Option<String>,
    rows: usize,
}

impl Piece {
    fn new(text: &str, seed: String, first: usize, first_id: Option<String>) -> Self {
        Self {
            source: Arc::from(text),
            bytes: 0..text.len(),
            seed,
            first,
            first_id,
            rows: text.bytes().filter(|byte| *byte == b'\n').count() + 1,
        }
    }

    fn text(&self) -> &str {
        &self.source[self.bytes.clone()]
    }

    fn id(&self, row: usize) -> String {
        if self.first + row == 0
            && let Some(id) = &self.first_id
        {
            return id.clone();
        }
        format!("{}:{}", self.seed, self.first + row)
    }

    fn row_start(&self, row: usize) -> usize {
        if row == 0 {
            0
        } else {
            self.text()
                .match_indices('\n')
                .nth(row - 1)
                .map_or(self.text().len(), |(at, _)| at + 1)
        }
    }

    fn slice(&self, rows: Range<usize>) -> Self {
        let start = self.row_start(rows.start);
        let end = if rows.end == self.rows {
            self.text().len()
        } else {
            self.row_start(rows.end) - 1
        };
        Self {
            source: Arc::clone(&self.source),
            bytes: self.bytes.start + start..self.bytes.start + end,
            seed: self.seed.clone(),
            first: self.first + rows.start,
            first_id: self.first_id.clone(),
            rows: rows.len(),
        }
    }
}

impl PartialEq for Piece {
    fn eq(&self, other: &Self) -> bool {
        self.rows == other.rows
            && self.text() == other.text()
            && ((self.seed == other.seed
                && self.first == other.first
                && self.first_id == other.first_id)
                || (self.rows == 1 && self.id(0) == other.id(0)))
    }
}
impl Eq for Piece {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPiece {
    text: String,
    seed: String,
    first: usize,
    #[serde(default)]
    first_id: Option<String>,
}

impl Serialize for Piece {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Piece", 4)?;
        state.serialize_field("text", self.text())?;
        state.serialize_field("seed", &self.seed)?;
        state.serialize_field("first", &self.first)?;
        state.serialize_field("first_id", &self.first_id)?;
        state.end()
    }
}

/// Text pieces, indexed by row counts rather than per-row records.
#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct Lines {
    pieces: Vec<Piece>,
    starts: Vec<usize>,
    len: usize,
}

impl std::fmt::Debug for Lines {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Lines")
            .field("rows", &self.len)
            .field("pieces", &self.pieces.len())
            .finish_non_exhaustive()
    }
}

impl Serialize for Lines {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("Lines", 2)?;
        state.serialize_field("version", &1u8)?;
        state.serialize_field("pieces", &self.pieces)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for Lines {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Compact {
            version: u8,
            pieces: Vec<StoredPiece>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Legacy(Vec<Line>),
            Compact(Compact),
        }
        match Stored::deserialize(deserializer)? {
            Stored::Legacy(lines) => Ok(Self::from(lines)),
            Stored::Compact(stored) => {
                if stored.version != 1 {
                    return Err(serde::de::Error::custom("Unknown draft text version"));
                }
                let mut lines = Self::default();
                for piece in stored.pieces {
                    let rows = piece.text.bytes().filter(|byte| *byte == b'\n').count() + 1;
                    if piece.seed.is_empty()
                        || piece.seed.len() > 128
                        || piece.seed.contains(':')
                        || piece.first.checked_add(rows).is_none()
                        || lines.len.checked_add(rows).is_none()
                    {
                        return Err(serde::de::Error::custom(
                            "Invalid draft text identity or row count",
                        ));
                    }
                    lines.push(Piece::new(
                        &piece.text,
                        piece.seed,
                        piece.first,
                        piece.first_id,
                    ));
                }
                Ok(lines)
            }
        }
    }
}

impl From<Vec<Line>> for Lines {
    fn from(lines: Vec<Line>) -> Self {
        let mut result = Self::default();
        for line in lines {
            result.push(Piece::new(
                &line.text,
                uuid::Uuid::new_v4().to_string(),
                0,
                Some(line.id),
            ));
        }
        result
    }
}

impl Lines {
    pub fn new(text: &str, first_id: Option<String>) -> Self {
        let mut result = Self::default();
        let mut start = 0;
        for (at, byte) in text.bytes().enumerate() {
            if byte == b'\n' && at + 1 - start >= CHUNK_BYTES {
                result.push(Piece::new(
                    &text[start..at],
                    uuid::Uuid::new_v4().to_string(),
                    0,
                    None,
                ));
                start = at + 1;
            }
        }
        result.push(Piece::new(
            &text[start..],
            uuid::Uuid::new_v4().to_string(),
            0,
            None,
        ));
        if let Some(piece) = result.pieces.first_mut() {
            piece.first_id = first_id;
        }
        result
    }

    fn push(&mut self, mut piece: Piece) {
        if piece.first != 0 {
            piece.first_id = None;
        }
        if let Some(previous) = self.pieces.last_mut()
            && previous.seed == piece.seed
            && previous.first + previous.rows == piece.first
        {
            if Arc::ptr_eq(&previous.source, &piece.source)
                && previous.bytes.end + 1 == piece.bytes.start
            {
                previous.bytes.end = piece.bytes.end;
                previous.rows += piece.rows;
                self.len += piece.rows;
                return;
            }
            if previous
                .text()
                .len()
                .saturating_add(piece.text().len())
                .saturating_add(1)
                <= CHUNK_BYTES
            {
                let text = format!("{}\n{}", previous.text(), piece.text());
                previous.source = Arc::from(text.as_str());
                previous.bytes = 0..text.len();
                previous.rows += piece.rows;
                self.len += piece.rows;
                return;
            }
        }
        self.starts.push(self.len);
        self.len += piece.rows;
        self.pieces.push(piece);
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub(super) fn identities(&self) -> Identities {
        let mut ids: Vec<Identity> = Vec::new();
        for piece in &self.pieces {
            if let Some(previous) = ids.last_mut()
                && previous.seed == piece.seed
                && previous.first.checked_add(previous.rows) == Some(piece.first)
            {
                previous.rows += piece.rows;
            } else {
                ids.push(Identity {
                    seed: piece.seed.clone(),
                    first: piece.first,
                    rows: piece.rows,
                    first_id: if piece.first == 0 {
                        piece.first_id.clone()
                    } else {
                        None
                    },
                });
            }
        }
        Identities(ids)
    }

    pub(super) fn with_identities(text: &str, identities: &Identities) -> Option<Self> {
        let parsed = Self::new(text, None);
        if identities.rows()? != parsed.len() {
            return None;
        }
        let mut result = Self::default();
        let mut start = 0usize;
        for identity in &identities.0 {
            let end = start.checked_add(identity.rows)?;
            let segment = parsed.slice(start..end)?;
            let mut ordinal = identity.first;
            for mut piece in segment.pieces {
                piece.seed = identity.seed.clone();
                piece.first = ordinal;
                piece.first_id = identity.first_id.clone();
                ordinal = ordinal.checked_add(piece.rows)?;
                result.push(piece);
            }
            start = end;
        }
        Some(result)
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn location(&self, row: usize) -> Option<(usize, usize)> {
        if row >= self.len {
            return None;
        }
        let index = self.starts.partition_point(|start| *start <= row) - 1;
        Some((index, row - self.starts[index]))
    }

    pub fn text_at(&self, row: usize) -> Option<&str> {
        let (piece, local) = self.location(row)?;
        self.pieces[piece].text().split('\n').nth(local)
    }

    pub fn get(&self, row: usize) -> Option<Line> {
        let (piece, local) = self.location(row)?;
        Some(Line {
            id: self.pieces[piece].id(local),
            text: self.text_at(row)?.into(),
        })
    }

    pub fn id(&self, row: usize) -> String {
        self.location(row)
            .map_or_else(String::new, |(piece, local)| self.pieces[piece].id(local))
    }

    #[cfg(test)]
    pub fn last(&self) -> Option<Line> {
        self.get(self.len.checked_sub(1)?)
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        let parsed = id.rsplit_once(':').and_then(|(seed, ordinal)| {
            ordinal.parse::<usize>().ok().map(|ordinal| (seed, ordinal))
        });
        for (index, piece) in self.pieces.iter().enumerate() {
            if piece.first == 0 && piece.first_id.as_deref() == Some(id) {
                return Some(self.starts[index]);
            }
            if let Some((seed, ordinal)) = parsed
                && seed == piece.seed
                && ordinal >= piece.first
                && ordinal - piece.first < piece.rows
                && !(ordinal == 0 && piece.first_id.is_some())
            {
                return Some(self.starts[index] + ordinal - piece.first);
            }
        }
        None
    }

    pub fn slice(&self, rows: Range<usize>) -> Option<Self> {
        if rows.start > rows.end || rows.end > self.len {
            return None;
        }
        let mut result = Self::default();
        for (index, piece) in self.pieces.iter().enumerate() {
            let start = rows
                .start
                .saturating_sub(self.starts[index])
                .min(piece.rows);
            let end = rows.end.saturating_sub(self.starts[index]).min(piece.rows);
            if start < end {
                result.push(piece.slice(start..end));
            }
        }
        Some(result)
    }

    pub fn splice(&mut self, rows: Range<usize>, replacement: Self) -> bool {
        let Some(mut result) = self.slice(0..rows.start) else {
            return false;
        };
        let Some(tail) = self.slice(rows.end..self.len) else {
            return false;
        };
        if rows.start > rows.end {
            return false;
        }
        for piece in replacement.pieces.into_iter().chain(tail.pieces) {
            result.push(piece);
        }
        *self = result;
        true
    }

    pub fn byte_len(&self) -> usize {
        self.pieces
            .iter()
            .map(|piece| piece.text().len())
            .sum::<usize>()
            + self.pieces.len().saturating_sub(1)
    }

    pub fn text(&self) -> String {
        let mut text = String::with_capacity(self.byte_len());
        for (index, piece) in self.pieces.iter().enumerate() {
            if index != 0 {
                text.push('\n');
            }
            text.push_str(piece.text());
        }
        text
    }

    pub fn retained_bytes(&self) -> usize {
        self.pieces.iter().fold(
            self.pieces.capacity() * std::mem::size_of::<Piece>()
                + self.starts.capacity() * std::mem::size_of::<usize>(),
            |bytes, piece| {
                bytes
                    .saturating_add(piece.source.len())
                    .saturating_add(piece.seed.capacity())
                    .saturating_add(piece.first_id.as_ref().map_or(0, String::capacity))
            },
        )
    }

    pub fn iter(&self) -> Rows<'_> {
        Rows {
            lines: self,
            next: 0,
        }
    }
}

pub(crate) struct Rows<'a> {
    lines: &'a Lines,
    next: usize,
}
impl Iterator for Rows<'_> {
    type Item = Line;
    fn next(&mut self) -> Option<Line> {
        let line = self.lines.get(self.next)?;
        self.next += 1;
        Some(line)
    }
    fn nth(&mut self, n: usize) -> Option<Line> {
        self.next = self.next.saturating_add(n).min(self.lines.len);
        self.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.lines.len.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for Rows<'_> {}

#[cfg(test)]
mod tests {
    #[test]
    fn compressed_identities_restore_rechunked_text_and_reject_invalid_counts() {
        let mut lines = Lines::new(&"row\n".repeat(20_000), Some("first-row".into()));
        let before = lines.identities();
        let restored = Lines::with_identities(&lines.text(), &before).unwrap();
        for index in [0, 1, 8192, lines.len() - 1] {
            assert_eq!(restored.id(index), lines.id(index));
        }
        assert!(
            before.bytes() < 4096,
            "Identity storage must scale with chunks, not rows"
        );
        assert!(before.matches(&restored));
        let mut corrupt = before.clone();
        corrupt.0[0].rows = usize::MAX;
        assert!(Lines::with_identities(&lines.text(), &corrupt).is_none());
        corrupt = before;
        corrupt.0[0].first = usize::MAX;
        assert!(Lines::with_identities(&lines.text(), &corrupt).is_none());
        let inserted = Lines::new("inserted", None);
        lines.splice(1..1, inserted);
        let changed = lines.identities();
        assert!(!changed.matches(&restored));
        let roundtrip = Lines::with_identities(&lines.text(), &changed).unwrap();
        assert_eq!(roundtrip, lines);
    }

    use super::*;

    #[test]
    fn chunked_rows_match_splices_and_recovery_without_changing_unaffected_ids() {
        let mut lines = Lines::new("first\n🦀\r\nlast\n", None);
        let mut expected: Vec<_> = lines.iter().collect();
        for step in 0..120 {
            let start = step % (expected.len() + 1);
            let end = (start + step % 3).min(expected.len());
            let before = lines.clone();
            let removed = lines.slice(start..end).unwrap();
            let old_expected = expected.clone();
            let text = match step % 4 {
                0 => "",
                1 => "one\n🦀",
                2 => "é\r\n",
                _ => "many\n\nrows",
            };
            let first = (start < end).then(|| expected[start].id.clone());
            let inserted = Lines::new(text, first);
            let count = inserted.len();
            expected.splice(start..end, inserted.iter());
            assert!(lines.splice(start..end, inserted));
            assert_eq!(lines.iter().collect::<Vec<_>>(), expected);
            for (index, row) in expected.iter().enumerate() {
                assert_eq!(lines.index_of(&row.id), Some(index));
            }
            let saved = serde_json::to_string(&lines).unwrap();
            let mut recovered: Lines = serde_json::from_str(&saved).unwrap();
            assert_eq!(recovered.iter().collect::<Vec<_>>(), expected);
            assert!(recovered.splice(start..start + count, removed));
            assert_eq!(recovered.iter().collect::<Vec<_>>(), old_expected);
            assert_eq!(recovered, before);
        }
    }

    #[test]
    fn chunked_rows_validate_versions_counts_and_legacy_identities() {
        let old = r#"[{"id":"old-first","text":"one"},{"id":"old-last","text":"🦀"}]"#;
        let lines: Lines = serde_json::from_str(old).unwrap();
        assert_eq!(lines.text(), "one\n🦀");
        assert_eq!(lines.index_of("old-last"), Some(1));
        for value in [
            serde_json::json!({"version":2,"pieces":[]}),
            serde_json::json!({"version":1,"pieces":[{"text":"x","seed":"bad:seed","first":0}]}),
            serde_json::json!({"version":1,"pieces":[{"text":"x","seed":"valid","first":usize::MAX}]}),
        ] {
            assert!(serde_json::from_value::<Lines>(value).is_err());
        }
        assert!(!format!("{lines:?}").contains("old-last"));
        assert!(!format!("{lines:?}").contains("🦀"));
    }
}
