//! Compact transcript grouping without copying hidden reference history.

// Rust guideline compliant 2026-02-21
use crate::model::Entry;
use std::borrow::Cow;

/// One rendered message, coalescing adjacent stream chunks with the same role.
pub(crate) struct Message<'a> {
    pub sequence: i64,
    pub role: &'a str,
    pub text: Cow<'a, str>,
    pub boundary: bool,
}

impl std::fmt::Debug for Message<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Message")
            .field("sequence", &self.sequence)
            .field("role", &self.role)
            .field("boundary", &self.boundary)
            .finish_non_exhaustive()
    }
}

/// Finds the last context boundary in the current loaded page.
pub(crate) fn latest_boundary(entries: &[Entry]) -> Option<usize> {
    entries.iter().rposition(Entry::is_context_boundary)
}

/// Coalesces visible chunks without merging across a context boundary.
pub(crate) fn messages(entries: &[Entry]) -> Vec<Message<'_>> {
    let mut messages: Vec<Message<'_>> = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| !entry.role.starts_with("aside"))
    {
        let boundary = entry.is_context_boundary();
        if let Some(last) = messages
            .last_mut()
            .filter(|last| !last.boundary && !boundary && last.role == entry.role)
        {
            last.text.to_mut().push_str(&entry.text);
        } else {
            messages.push(Message {
                sequence: entry.sequence,
                role: &entry.role,
                text: Cow::Borrowed(&entry.text),
                boundary,
            });
        }
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::{latest_boundary, messages};
    use crate::model::Entry;

    fn entry(sequence: i64, role: &str, text: &str) -> Entry {
        Entry {
            sequence,
            session_id: "session".into(),
            role: role.into(),
            text: text.into(),
        }
    }

    #[test]
    fn transcript_boundaries_isolate_stream_chunks_and_latest_context() {
        let entries = [
            entry(1, "assistant", "old"),
            entry(2, "context_boundary", "reset"),
            entry(3, "system", "first status"),
            entry(
                4,
                "system",
                "Plank replaced its conversation; earlier messages are retained for reference.",
            ),
            entry(5, "system", "second status"),
            entry(6, "assistant", "Hello "),
            entry(7, "aside", "separate question"),
            entry(8, "assistant", "🦀"),
        ];
        assert_eq!(latest_boundary(&entries), Some(3));
        let current = messages(&entries[latest_boundary(&entries).unwrap()..]);
        assert_eq!(current.len(), 3);
        assert!(current[0].boundary);
        assert_eq!(current[1].text, "second status");
        assert_eq!(current[2].text, "Hello 🦀");
        assert_eq!(current[2].sequence, 6);
        let all = messages(&entries);
        assert_eq!(all.iter().filter(|message| message.boundary).count(), 2);
        assert!(all.iter().any(|message| message.text == "old"));
        assert!(!format!("{:?}", current[2]).contains("Hello"));
    }

    #[test]
    fn transcript_ordinary_text_cannot_impersonate_a_context_boundary() {
        let marker =
            "A fresh engine context starts here; earlier messages are retained for reference.";
        assert!(latest_boundary(&[entry(1, "assistant", marker)]).is_none());
        assert!(latest_boundary(&[entry(1, "user", marker)]).is_none());
        assert_eq!(latest_boundary(&[entry(1, "system", marker)]), Some(0));
        assert!(latest_boundary(&[]).is_none());
        assert!(messages(&[]).is_empty());
        let entries = [
            entry(1, "context_boundary", "one"),
            entry(2, "context_boundary", "two"),
        ];
        assert_eq!(latest_boundary(&entries), Some(1));
        assert_eq!(messages(&entries).len(), 2);
    }

    #[test]
    #[cfg(not(target_family = "wasm"))]
    fn transcript_context_boundaries_survive_reopen_and_keep_older_pages_accessible()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let mut store = crate::store::Store::open(&path)?;
        let workspace = store.create_workspace("Local", directory.path().to_str().unwrap())?;
        let id = store.create_session(
            &workspace,
            "Agent",
            crate::model::Backend::Local {
                executable: "unused".into(),
                args: Vec::new(),
            },
        )?;
        store.append(&id, "user", "old prompt")?;
        store.append(&id, "assistant", "old answer")?;
        let generation = store.session(&id)?.generation;
        assert!(store.conversation_replaced(&id, generation, Some(14))?);
        assert!(!store.conversation_replaced(&id, generation, Some(14))?);
        store.append(&id, "assistant", "retained history")?;
        store.reset(&id)?;
        let saved = store.snapshot(Some(&id), 0)?.entries;
        assert_eq!(
            saved
                .iter()
                .filter(|entry| entry.is_context_boundary())
                .count(),
            2
        );
        assert_eq!(saved[0].text, "old prompt");
        assert_eq!(saved[1].text, "old answer");
        assert!(
            saved
                .iter()
                .filter(|entry| entry.is_context_boundary())
                .all(|entry| entry.role == "context_boundary")
        );
        let boundary = saved[latest_boundary(&saved).unwrap()].sequence;
        let older = store.snapshot(Some(&id), boundary)?.entries;
        assert!(older.iter().any(|entry| entry.text == "old answer"));
        assert!(older.iter().any(|entry| entry.text == "retained history"));
        drop(store);
        let store = crate::store::Store::open(&path)?;
        assert_eq!(store.snapshot(Some(&id), 0)?.entries, saved);
        Ok(())
    }
}
