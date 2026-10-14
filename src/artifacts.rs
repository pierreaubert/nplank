//! Stages complete engine exports in anonymous files and verifies their integrity.
//!
//! File content stays out of snapshots and SQLite. One pending and one completed
//! file per category are retained per session generation. Anonymous temporary files close on
//! failure, reset, daemon exit or process death. Concurrent readers use independent
//! byte offsets under the file lock rather than sharing a cloned descriptor cursor.

// Rust guideline compliant 2026-02-21
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Result, ensure};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::EngineExport;

/// Matches the engine's chunk ceiling; total exports are not frame-size limited.
const CHUNK_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub(crate) enum ArtifactEvent {
    Begin {
        #[serde(default)]
        purpose: crate::model::ExportPurpose,
        id: String,
        name: String,
        format: String,
        bytes: u64,
        sha256: String,
    },
    Chunk {
        id: String,
        offset: u64,
        text: String,
    },
    End {
        id: String,
        bytes: u64,
    },
    Abort {
        id: String,
        reason: String,
    },
}

impl std::fmt::Debug for ArtifactEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Begin { id, bytes, .. } => f
                .debug_struct("ExportBegin")
                .field("id", id)
                .field("bytes", bytes)
                .finish_non_exhaustive(),
            Self::Chunk { id, offset, text } => f
                .debug_struct("ExportChunk")
                .field("id", id)
                .field("offset", offset)
                .field("bytes", &text.len())
                .finish_non_exhaustive(),
            Self::End { id, bytes } => f
                .debug_struct("ExportEnd")
                .field("id", id)
                .field("bytes", bytes)
                .finish(),
            Self::Abort { id, .. } => f
                .debug_struct("ExportAbort")
                .field("id", id)
                .finish_non_exhaustive(),
        }
    }
}

struct Pending {
    id: String,
    metadata: EngineExport,
    written: u64,
    hash: Sha256,
    file: File,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Hash implementations may retain an unprocessed plaintext block.
        f.debug_struct("PendingExport")
            .field("id", &self.id)
            .field("written", &self.written)
            .field("expected", &self.metadata.bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Completed {
    metadata: EngineExport,
    file: Arc<Mutex<File>>,
}

#[derive(Debug)]
struct SessionArtifacts {
    generation: u64,
    pending: Option<Pending>,
    completed: HashMap<crate::model::ExportPurpose, Completed>,
}

#[derive(Debug)]
pub(crate) struct Artifacts {
    directory: PathBuf,
    sessions: HashMap<String, SessionArtifacts>,
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

impl Artifacts {
    pub(crate) fn new(directory: &Path) -> Result<Self> {
        ensure!(
            directory.is_dir(),
            "Export staging directory is unavailable"
        );
        Ok(Self {
            directory: directory.into(),
            sessions: HashMap::new(),
        })
    }

    /// Accepts a contiguous event; invalid active transfers are discarded.
    pub(crate) fn ingest(
        &mut self,
        session: &str,
        generation: u64,
        event: ArtifactEvent,
    ) -> Result<Option<EngineExport>> {
        let state = self
            .sessions
            .entry(session.into())
            .or_insert_with(|| SessionArtifacts {
                generation,
                pending: None,
                completed: HashMap::new(),
            });
        ensure!(generation >= state.generation, "Stale export generation");
        if generation > state.generation {
            *state = SessionArtifacts {
                generation,
                pending: None,
                completed: HashMap::new(),
            };
        }
        if let ArtifactEvent::Abort { id, reason } = event {
            ensure!(
                valid_id(&id) && reason.len() <= 512 && !reason.chars().any(char::is_control),
                "Invalid export retirement"
            );
            if state
                .pending
                .as_ref()
                .is_some_and(|pending| pending.id == id)
            {
                state.pending = None;
            }
            return Ok(None);
        }
        if let ArtifactEvent::Begin {
            purpose,
            id,
            name,
            format,
            bytes,
            sha256,
        } = event
        {
            // A new begin abandons any previous partial transfer, even if invalid.
            state.pending = None;
            ensure!(valid_id(&id), "Invalid export identity");
            ensure!(
                !name.is_empty()
                    && name.len() <= 255
                    && name != "."
                    && name != ".."
                    && !name
                        .chars()
                        .any(|c| c.is_control() || matches!(c, '/' | '\\')),
                "Invalid export filename"
            );
            ensure!(
                matches!(format.as_str(), "md" | "html"),
                "Invalid export format"
            );
            ensure!(
                purpose != crate::model::ExportPurpose::Insights || format == "html",
                "Insights artifact must be HTML"
            );
            ensure!(
                sha256.len() == 64
                    && sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "Invalid export checksum"
            );
            let file = tempfile::tempfile_in(&self.directory)?;
            state.pending = Some(Pending {
                id,
                metadata: EngineExport {
                    purpose,
                    id: uuid::Uuid::new_v4().to_string(),
                    name,
                    format,
                    bytes,
                    sha256,
                },
                written: 0,
                hash: Sha256::new(),
                file,
            });
            return Ok(None);
        }
        // Taking ownership ensures any failed validation or write drops the file.
        let mut pending = state
            .pending
            .take()
            .ok_or_else(|| anyhow::anyhow!("Export has no begin frame"))?;
        match event {
            ArtifactEvent::Chunk { id, offset, text } => {
                ensure!(
                    id == pending.id && offset == pending.written,
                    "Export chunk identity or offset mismatch"
                );
                ensure!(
                    !text.is_empty() && text.len() <= CHUNK_BYTES,
                    "Invalid export chunk size"
                );
                let next = offset
                    .checked_add(text.len() as u64)
                    .ok_or_else(|| anyhow::anyhow!("Export length overflow"))?;
                ensure!(
                    next <= pending.metadata.bytes,
                    "Export exceeds its declared length"
                );
                pending.file.write_all(text.as_bytes())?;
                pending.hash.update(text.as_bytes());
                pending.written = next;
                state.pending = Some(pending);
                Ok(None)
            }
            ArtifactEvent::End { id, bytes } => {
                ensure!(
                    id == pending.id && bytes == pending.written && bytes == pending.metadata.bytes,
                    "Export completion length or identity mismatch"
                );
                ensure!(
                    hex(&pending.hash.finalize()) == pending.metadata.sha256,
                    "Export checksum mismatch"
                );
                pending.file.flush()?;
                let metadata = pending.metadata;
                state.completed.insert(
                    metadata.purpose,
                    Completed {
                        metadata: metadata.clone(),
                        file: Arc::new(Mutex::new(pending.file)),
                    },
                );
                Ok(Some(metadata))
            }
            ArtifactEvent::Begin { .. } => {
                unreachable!("begin frames return before taking pending state")
            }
            ArtifactEvent::Abort { .. } => {
                unreachable!("abort frames return before taking pending state")
            }
        }
    }

    pub(crate) fn clear(&mut self, session: &str, generation: u64) {
        if self
            .sessions
            .get(session)
            .is_some_and(|state| state.generation <= generation)
        {
            self.sessions.remove(session);
        }
    }

    pub(crate) fn discard_pending(&mut self, session: &str, generation: u64) {
        if let Some(state) = self
            .sessions
            .get_mut(session)
            .filter(|state| state.generation == generation)
        {
            state.pending = None;
        }
    }

    pub(crate) fn retain_generation(&mut self, session: &str, generation: u64) {
        if self
            .sessions
            .get(session)
            .is_some_and(|state| state.generation < generation)
        {
            self.sessions.remove(session);
        }
    }

    pub(crate) fn owns_pending(&self, session: &str, generation: u64, id: &str) -> bool {
        self.sessions.get(session).is_some_and(|state| {
            state.generation == generation
                && state
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == id)
        })
    }

    pub(crate) fn file(
        &self,
        session: &str,
        generation: u64,
        id: &str,
    ) -> Option<(EngineExport, Arc<Mutex<File>>)> {
        let state = self
            .sessions
            .get(session)
            .filter(|state| state.generation == generation)?;
        let completed = state
            .completed
            .values()
            .find(|file| file.metadata.id == id)?;
        Some((completed.metadata.clone(), Arc::clone(&completed.file)))
    }

    /// Reads only verified descriptors owned by the requested worker generation.
    pub(crate) fn metadata(&self, session: &str, generation: u64) -> Vec<EngineExport> {
        self.sessions
            .get(session)
            .filter(|state| state.generation == generation)
            .map(|state| {
                state
                    .completed
                    .values()
                    .map(|file| file.metadata.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Reads a bounded chunk using an independent cursor for each download.
pub(crate) fn read(file: &Mutex<File>, offset: u64) -> std::io::Result<Vec<u8>> {
    let mut file = file
        .lock()
        .map_err(|_| std::io::Error::other("Export file lock poisoned"))?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; CHUNK_BYTES];
    let count = file.read(&mut bytes)?;
    bytes.truncate(count);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn begin(body: &str) -> ArtifactEvent {
        ArtifactEvent::Begin {
            purpose: crate::model::ExportPurpose::Conversation,
            id: "export-1".into(),
            name: "conversation.html".into(),
            format: "html".into(),
            bytes: body.len() as u64,
            sha256: hex(&Sha256::digest(body.as_bytes())),
        }
    }

    fn complete(artifacts: &mut Artifacts, generation: u64, body: &str) -> EngineExport {
        artifacts
            .ingest("session", generation, begin(body))
            .unwrap();
        let mut offset = 0;
        while offset < body.len() {
            let mut end = (offset + CHUNK_BYTES).min(body.len());
            while !body.is_char_boundary(end) {
                end -= 1;
            }
            artifacts
                .ingest(
                    "session",
                    generation,
                    ArtifactEvent::Chunk {
                        id: "export-1".into(),
                        offset: offset as u64,
                        text: body[offset..end].into(),
                    },
                )
                .unwrap();
            offset = end;
        }
        artifacts
            .ingest(
                "session",
                generation,
                ArtifactEvent::End {
                    id: "export-1".into(),
                    bytes: body.len() as u64,
                },
            )
            .unwrap()
            .unwrap()
    }

    #[test]
    fn export_staging_verifies_complete_files_and_readers_have_independent_offsets() {
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::new(dir.path()).unwrap();
        let body = "🦀\r\n<private>".repeat(250_000);
        let metadata = complete(&mut artifacts, 7, &body);
        let (_, first) = artifacts.file("session", 7, &metadata.id).unwrap();
        let (_, second) = artifacts.file("session", 7, &metadata.id).unwrap();
        let head = read(&first, 0).unwrap();
        let next = read(&second, CHUNK_BYTES as u64).unwrap();
        assert_eq!(head, body.as_bytes()[..CHUNK_BYTES]);
        assert_eq!(next, body.as_bytes()[CHUNK_BYTES..CHUNK_BYTES * 2]);
        assert_eq!(read(&first, 0).unwrap(), head);
        assert!(artifacts.file("session", 8, &metadata.id).is_none());
        artifacts.clear("session", 7);
        assert!(artifacts.file("session", 7, &metadata.id).is_none());
    }

    #[test]
    fn export_staging_rejects_missing_duplicate_corrupt_and_oversized_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::new(dir.path()).unwrap();
        for event in [
            ArtifactEvent::Chunk {
                id: "export-1".into(),
                offset: 1,
                text: "abc".into(),
            },
            ArtifactEvent::Chunk {
                id: "wrong".into(),
                offset: 0,
                text: "abc".into(),
            },
            ArtifactEvent::Chunk {
                id: "export-1".into(),
                offset: 0,
                text: "abcd".into(),
            },
            ArtifactEvent::Chunk {
                id: "export-1".into(),
                offset: 0,
                text: "x".repeat(CHUNK_BYTES + 1),
            },
            ArtifactEvent::End {
                id: "export-1".into(),
                bytes: 3,
            },
        ] {
            artifacts.ingest("session", 1, begin("abc")).unwrap();
            assert!(artifacts.ingest("session", 1, event).is_err());
            assert!(artifacts.sessions["session"].pending.is_none());
        }
        artifacts.ingest("session", 1, begin("abc")).unwrap();
        artifacts
            .ingest(
                "session",
                1,
                ArtifactEvent::Chunk {
                    id: "export-1".into(),
                    offset: 0,
                    text: "abd".into(),
                },
            )
            .unwrap();
        assert!(
            artifacts
                .ingest(
                    "session",
                    1,
                    ArtifactEvent::End {
                        id: "export-1".into(),
                        bytes: 3
                    }
                )
                .is_err()
        );
        assert!(artifacts.sessions["session"].pending.is_none());
        artifacts.ingest("session", 1, begin("abc")).unwrap();
        let chunk = || ArtifactEvent::Chunk {
            id: "export-1".into(),
            offset: 0,
            text: "a".into(),
        };
        artifacts.ingest("session", 1, chunk()).unwrap();
        assert!(artifacts.ingest("session", 1, chunk()).is_err());
    }

    #[test]
    fn export_staging_preserves_verified_files_and_fences_old_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::new(dir.path()).unwrap();
        let old = complete(&mut artifacts, 1, "first");
        artifacts.ingest("session", 1, begin("second")).unwrap();
        artifacts.discard_pending("session", 1);
        assert!(artifacts.file("session", 1, &old.id).is_some());
        artifacts.ingest("session", 1, begin("second")).unwrap();
        artifacts
            .ingest(
                "session",
                1,
                ArtifactEvent::Abort {
                    id: "export-1".into(),
                    reason: "reader stalled".into(),
                },
            )
            .unwrap();
        assert!(artifacts.sessions["session"].pending.is_none());
        assert!(artifacts.file("session", 1, &old.id).is_some());
        let new = complete(&mut artifacts, 2, "");
        assert!(artifacts.ingest("session", 1, begin("stale")).is_err());
        artifacts.clear("session", 1);
        artifacts.discard_pending("session", 1);
        assert!(artifacts.file("session", 2, &new.id).is_some());
        assert!(artifacts.file("session", 1, &old.id).is_none());
    }

    #[test]
    fn export_staging_debug_omits_file_contents_and_hash_buffers() {
        let chunk = ArtifactEvent::Chunk {
            id: "export-1".into(),
            offset: 0,
            text: "secret document".into(),
        };
        assert!(!format!("{chunk:?}").contains("secret"));
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::new(dir.path()).unwrap();
        artifacts
            .ingest("session", 1, begin("secret document"))
            .unwrap();
        artifacts.ingest("session", 1, chunk).unwrap();
        let debug = format!("{:?}", artifacts.sessions["session"].pending);
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("hash"));
    }

    #[test]
    fn export_staging_keeps_insights_and_conversation_files_independently() {
        let dir = tempfile::tempdir().unwrap();
        let mut artifacts = Artifacts::new(dir.path()).unwrap();
        let conversation = complete(&mut artifacts, 1, "conversation");
        let mut report = begin("report");
        if let ArtifactEvent::Begin { purpose, .. } = &mut report {
            *purpose = crate::model::ExportPurpose::Insights;
        }
        artifacts.ingest("session", 1, report).unwrap();
        artifacts
            .ingest(
                "session",
                1,
                ArtifactEvent::Chunk {
                    id: "export-1".into(),
                    offset: 0,
                    text: "report".into(),
                },
            )
            .unwrap();
        let report = artifacts
            .ingest(
                "session",
                1,
                ArtifactEvent::End {
                    id: "export-1".into(),
                    bytes: 6,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(report.purpose, crate::model::ExportPurpose::Insights);
        let (_, file) = artifacts.file("session", 1, &report.id).unwrap();
        assert_eq!(read(&file, 0).unwrap(), b"report");
        let newer = complete(&mut artifacts, 1, "new conversation");
        assert!(artifacts.file("session", 1, &conversation.id).is_none());
        assert!(artifacts.file("session", 1, &newer.id).is_some());
        assert!(artifacts.file("session", 1, &report.id).is_some());
        assert_eq!(artifacts.sessions["session"].completed.len(), 2);
        assert_eq!(artifacts.metadata("session", 1).len(), 2);
        assert!(artifacts.metadata("session", 2).is_empty());
        artifacts.clear("session", 1);
        assert!(artifacts.metadata("session", 1).is_empty());
    }
}
