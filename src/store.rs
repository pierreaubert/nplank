//! SQLite transactions preserve accepted prompts and transcripts across process failures.

// Rust guideline compliant 2026-02-21
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{Backend, Entry, RunState, Session, Snapshot, Workspace};

/// A single-writer durable workspace database.
#[derive(Debug)]
pub struct Store {
    db: Connection,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl Store {
    /// Opens a database and initializes its schema.
    ///
    /// # Errors
    /// Returns filesystem, SQLite, or schema errors.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        ensure!(
            version <= 3,
            "Database was created by a newer nplank version"
        );
        db.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS workspaces(id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, data TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS entries(sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL, role TEXT NOT NULL, text TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS entries_session ON entries(session_id, sequence);
             CREATE TABLE IF NOT EXISTS requests(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, text TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS remote_seen(session_id TEXT NOT NULL, id INTEGER NOT NULL,
                 PRIMARY KEY(session_id, id));
             CREATE TABLE IF NOT EXISTS remote_hosts(session_id TEXT PRIMARY KEY, instance TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS editor_documents(session_id TEXT NOT NULL, approval_id TEXT NOT NULL,
                 requirement TEXT NOT NULL, PRIMARY KEY(session_id, approval_id));
             CREATE TABLE IF NOT EXISTS editor_document_chunks(session_id TEXT NOT NULL, approval_id TEXT NOT NULL,
                 offset INTEGER NOT NULL, bytes BLOB NOT NULL, PRIMARY KEY(session_id, approval_id, offset));",
        )?;
        let store = Self { db };
        let mut statement = store
            .db
            .prepare("SELECT id,data,rowid FROM sessions ORDER BY rowid")?;
        let rows: Vec<(String, String, i64)> = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(statement);
        let migration = store.db.unchecked_transaction()?;
        for (id, data, rowid) in rows {
            let value: serde_json::Value = serde_json::from_str(&data)?;
            let missing_position = value.get("position").is_none();
            let mut session: Session = serde_json::from_value(value)?;
            let mut modified = missing_position;
            if missing_position {
                session.position = rowid;
            }
            for approval in &mut session.approvals {
                if approval.requirement["kind"] == "plank_editor" && approval.document.is_none() {
                    store.stage_document(&id, approval)?;
                    modified = true;
                }
            }
            if modified {
                store
                    .save_session(&session)
                    .with_context(|| format!("Cannot migrate session {id}"))?;
            }
        }
        // Finish cleanup after a prior crash between metadata retirement and
        // chunk deletion; unreferenced originals must not accumulate forever.
        store.db.execute_batch(
            "DELETE FROM editor_documents WHERE NOT EXISTS (
                SELECT 1 FROM sessions, json_each(sessions.data,'$.approvals') AS approval
                WHERE sessions.id=editor_documents.session_id
                  AND json_extract(approval.value,'$.id')=editor_documents.approval_id
                  AND json_extract(approval.value,'$.document') IS NOT NULL);
             DELETE FROM editor_document_chunks WHERE NOT EXISTS (
                SELECT 1 FROM editor_documents
                WHERE editor_documents.session_id=editor_document_chunks.session_id
                  AND editor_documents.approval_id=editor_document_chunks.approval_id);",
        )?;
        store.check_version()?;
        migration.commit()?;
        Ok(store)
    }

    /// Creates a workspace after checking its name and directory.
    ///
    /// # Errors
    /// Returns validation or database errors.
    pub fn create_workspace(&self, name: &str, directory: &str) -> Result<String> {
        ensure!(!name.trim().is_empty(), "Workspace name is required");
        ensure!(
            Path::new(directory).is_dir(),
            "Working directory does not exist on the daemon host"
        );
        let workspace = Workspace {
            id: Uuid::new_v4().to_string(),
            name: name.trim().into(),
            directory: std::fs::canonicalize(directory)?
                .to_string_lossy()
                .into_owned(),
        };
        self.db.execute(
            "INSERT INTO workspaces VALUES(?1, ?2)",
            params![workspace.id, serde_json::to_string(&workspace)?],
        )?;
        Ok(workspace.id)
    }

    /// Creates a session in an existing workspace.
    ///
    /// # Errors
    /// Returns validation or database errors.
    pub fn create_session(
        &self,
        workspace_id: &str,
        name: &str,
        backend: Backend,
    ) -> Result<String> {
        self.workspace(workspace_id)?;
        ensure!(!name.trim().is_empty(), "Session name is required");
        if let Backend::Local { executable, args }
        | Backend::Agent {
            executable, args, ..
        } = &backend
        {
            ensure!(
                !executable.trim().is_empty(),
                "Agent executable is required"
            );
            ensure!(
                args.len() <= 128 && args.iter().all(|a| a.len() < 16 * 1024),
                "Too many or oversized runner arguments"
            );
        }
        if let Backend::Local { args, .. } = &backend {
            ensure!(
                !args
                    .iter()
                    .any(|arg| matches!(arg.as_str(), "-p" | "--prompt" | "--ui")),
                "Do not include --ui or --prompt in engine arguments"
            );
        }
        if let Backend::Agent { args, .. } = &backend {
            for arg in args {
                let flag = arg.split('=').next().unwrap_or(arg);
                ensure!(
                    !matches!(
                        flag,
                        "--json"
                            | "--output-format"
                            | "--include-partial-messages"
                            | "--session-id"
                            | "--resume"
                            | "--continue"
                            | "--ephemeral"
                    ),
                    "The runner manages protocol and conversation flags: {flag}"
                );
            }
        }
        if let Backend::Remote { url, token_env } = &backend {
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                parsed.scheme() == "ws"
                    && matches!(
                        parsed.host_str(),
                        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
                    )
                    && parsed.username().is_empty(),
                "Use a loopback ws:// URL through an SSH tunnel"
            );
            ensure!(
                !token_env.is_empty(),
                "Remote token environment variable is required"
            );
        }
        let session = Session {
            progress: None,
            controls: None,
            attention: false,
            started_at: None,
            completed_at: None,
            summary: String::new(),
            id: Uuid::new_v4().to_string(),
            workspace_id: workspace_id.into(),
            name: name.trim().into(),
            backend,
            state: RunState::Idle,
            recovery_required: false,
            generation: 0,
            archived: false,
            position: self
                .snapshot(None, 0)?
                .sessions
                .iter()
                .map(|s| s.position)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
            agent_session: None,
            resumable: false,
            approvals: vec![],
        };
        self.db.execute(
            "INSERT INTO sessions VALUES(?1, ?2)",
            params![session.id, serde_json::to_string(&session)?],
        )?;
        Ok(session.id)
    }

    /// Reads a session by stable identifier.
    ///
    /// # Errors
    /// Returns an error for an unknown session or unreadable data.
    pub fn session(&self, id: &str) -> Result<Session> {
        let data: String = self
            .db
            .query_row("SELECT data FROM sessions WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .context("Unknown session")?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Reads a workspace by stable identifier.
    ///
    /// # Errors
    /// Returns an error for an unknown workspace or unreadable data.
    pub fn workspace(&self, id: &str) -> Result<Workspace> {
        let data: String = self
            .db
            .query_row("SELECT data FROM workspaces WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .context("Unknown workspace")?;
        Ok(serde_json::from_str(&data)?)
    }

    fn save_session(&self, session: &Session) -> Result<()> {
        let data = serde_json::to_string(session)?;
        self.db.execute(
            "UPDATE sessions SET data=?2 WHERE id=?1",
            params![session.id, data],
        )?;
        self.db.execute(
            "DELETE FROM editor_documents WHERE session_id=?1 AND approval_id NOT IN (
                SELECT json_extract(value,'$.id') FROM json_each(?2,'$.approvals')
                WHERE json_extract(value,'$.document') IS NOT NULL)",
            params![session.id, data],
        )?;
        self.db.execute(
            "DELETE FROM editor_document_chunks WHERE session_id=?1 AND approval_id NOT IN (
                SELECT approval_id FROM editor_documents WHERE session_id=?1)",
            [&session.id],
        )?;
        Ok(())
    }

    fn stage_document(
        &self,
        session_id: &str,
        approval: &mut crate::model::PendingApproval,
    ) -> Result<()> {
        ensure!(
            approval.arguments.len() <= crate::documenttransfer::DOCUMENT_BYTES,
            "Document exceeds the editing limit"
        );
        approval.document = Some(crate::model::EditorDocument {
            bytes: approval.arguments.len(),
            sha256: crate::documenttransfer::digest(approval.arguments.as_bytes()),
        });
        self.db.execute(
            "INSERT OR REPLACE INTO editor_documents VALUES(?1,?2,?3)",
            params![
                session_id,
                approval.id,
                serde_json::to_string(&approval.requirement)?
            ],
        )?;
        self.db.execute(
            "DELETE FROM editor_document_chunks WHERE session_id=?1 AND approval_id=?2",
            params![session_id, approval.id],
        )?;
        let mut insert = self
            .db
            .prepare("INSERT INTO editor_document_chunks VALUES(?1,?2,?3,?4)")?;
        for (index, bytes) in approval
            .arguments
            .as_bytes()
            .chunks(crate::model::EDITOR_FETCH_BYTES)
            .enumerate()
        {
            insert.execute(params![
                session_id,
                approval.id,
                i64::try_from(index * crate::model::EDITOR_FETCH_BYTES)?,
                bytes
            ])?;
        }
        approval.arguments.clear();
        Ok(())
    }

    /// Reads one bounded original-document chunk for the current editor requirement.
    ///
    /// # Errors
    /// Returns stale-revision, missing-document, size, or database errors.
    pub fn document_chunk(
        &self,
        session_id: &str,
        approval_id: &str,
        requirement: &serde_json::Value,
        offset: usize,
    ) -> Result<Vec<u8>> {
        let session = self.session(session_id)?;
        let editor = session
            .approvals
            .iter()
            .find(|editor| {
                editor.id == approval_id
                    && editor.requirement == *requirement
                    && editor.requirement["kind"] == "plank_editor"
            })
            .context("Editor changed; reopen before loading")?;
        let document = editor
            .document
            .as_ref()
            .context("Document metadata is unavailable")?;
        ensure!(
            document.bytes <= crate::documenttransfer::DOCUMENT_BYTES,
            "Document exceeds the editing limit"
        );
        ensure!(
            offset <= document.bytes && offset.is_multiple_of(crate::model::EDITOR_FETCH_BYTES),
            "Invalid document chunk offset"
        );
        let current: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM editor_documents WHERE session_id=?1 AND approval_id=?2 AND requirement=?3)",
            params![session_id, approval_id, serde_json::to_string(requirement)?], |row| row.get(0),
        )?;
        ensure!(current, "Original document is no longer available");
        if offset == document.bytes {
            return Ok(Vec::new());
        }
        let bytes: Vec<u8> = self.db.query_row(
            "SELECT bytes FROM editor_document_chunks WHERE session_id=?1 AND approval_id=?2 AND offset=?3",
            params![session_id, approval_id, i64::try_from(offset)?], |row| row.get(0),
        )?;
        ensure!(
            bytes.len() == (document.bytes - offset).min(crate::model::EDITOR_FETCH_BYTES),
            "Original document length changed"
        );
        Ok(bytes)
    }

    /// Changes the session's execution state.
    ///
    /// # Errors
    /// Returns database or unknown-session errors.
    pub fn set_state(&self, id: &str, state: RunState, recovery_required: bool) -> Result<()> {
        let mut session = self.session(id)?;
        if session.state.busy() && state == RunState::Idle {
            session.attention = true;
            session.completed_at = Some(now());
        }
        if matches!(state, RunState::Failed | RunState::Interrupted) {
            session.attention = true;
        }
        session.state = state;
        session.recovery_required = recovery_required;
        if recovery_required {
            session.controls = None;
        }
        if !state.busy() {
            session.approvals.clear();
            session.progress = None;
        }
        self.save_session(&session)
    }

    /// Moves an idle session to another workspace.
    ///
    /// # Errors
    /// Returns errors for busy sessions or unknown workspaces.
    pub fn move_session(&self, id: &str, workspace_id: &str) -> Result<()> {
        self.workspace(workspace_id)?;
        let mut session = self.session(id)?;
        ensure!(!session.state.busy(), "Stop the run before moving it");
        session.workspace_id = workspace_id.into();
        self.save_session(&session)
    }

    /// Renames a session without changing its identity.
    ///
    /// # Errors
    /// Returns errors for empty names or unknown sessions.
    pub fn rename_session(&self, id: &str, name: &str) -> Result<()> {
        ensure!(!name.trim().is_empty(), "Session name is required");
        let mut session = self.session(id)?;
        session.name = name.trim().into();
        self.save_session(&session)
    }

    /// Renames a workspace without changing its working directory.
    ///
    /// # Errors
    /// Returns validation, unknown-workspace, or database errors.
    pub fn rename_workspace(&self, id: &str, name: &str) -> Result<()> {
        ensure!(!name.trim().is_empty(), "Workspace name is required");
        let mut workspace = self.workspace(id)?;
        workspace.name = name.trim().into();
        self.db.execute(
            "UPDATE workspaces SET data=?2 WHERE id=?1",
            params![id, serde_json::to_string(&workspace)?],
        )?;
        Ok(())
    }

    /// Archives or restores a stopped session without deleting its history.
    ///
    /// # Errors
    /// Returns busy-session or database errors.
    pub fn archive(&self, id: &str, archived: bool) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(!session.state.busy(), "Stop the session before archiving");
        session.archived = archived;
        self.save_session(&session)
    }

    /// Changes the ordering of a tab within its workspace.
    ///
    /// # Errors
    /// Returns unknown-session or database errors.
    pub fn reorder(&self, id: &str, position: i64) -> Result<()> {
        let mut session = self.session(id)?;
        let peer = self.snapshot(None, 0)?.sessions.into_iter().find(|s| {
            s.id != id && s.workspace_id == session.workspace_id && s.position == position
        });
        let transaction = self.db.unchecked_transaction()?;
        if let Some(mut peer) = peer {
            peer.position = session.position;
            transaction.execute(
                "UPDATE sessions SET data=?2 WHERE id=?1",
                params![peer.id, serde_json::to_string(&peer)?],
            )?;
        }
        session.position = position;
        transaction.execute(
            "UPDATE sessions SET data=?2 WHERE id=?1",
            params![session.id, serde_json::to_string(&session)?],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Saves a provider conversation identity before accepting later turns.
    ///
    /// # Errors
    /// Returns stale-worker or database errors.
    pub fn agent_identity(
        &self,
        id: &str,
        generation: u64,
        identity: &str,
        resumable: bool,
    ) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Worker was replaced");
        ensure!(
            !identity.is_empty(),
            "Agent returned an empty conversation identifier"
        );
        session.agent_session = Some(identity.into());
        session.resumable = resumable;
        self.save_session(&session)
    }

    /// Publishes runtime controls only for the current engine generation.
    ///
    /// # Errors
    /// Returns stale-worker, invalid-catalog, or database errors.
    pub fn controls(
        &self,
        id: &str,
        generation: u64,
        controls: crate::commands::EngineControls,
    ) -> Result<()> {
        ensure!(controls.version == 1, "Unsupported engine control protocol");
        ensure!(
            controls
                .suggestion
                .as_ref()
                .is_none_or(crate::commands::PromptSuggestion::is_valid),
            "Invalid prompt suggestion"
        );
        ensure!(controls.features.len() <= 64, "Too many engine features");
        let mut features = std::collections::HashSet::new();
        for feature in &controls.features {
            ensure!(
                !feature.name.is_empty()
                    && feature.name.len() <= 64
                    && feature
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && features.insert(&feature.name)
                    && feature.label.len() <= 64
                    && feature.detail.len() <= 512
                    && !feature.label.chars().any(char::is_control)
                    && !feature.detail.chars().any(char::is_control),
                "Invalid engine feature metadata"
            );
        }
        ensure!(
            controls
                .settings
                .as_ref()
                .is_none_or(|settings| settings.is_valid()),
            "Invalid engine settings"
        );
        ensure!(
            controls.commands.len() <= 1024,
            "Engine catalog is too large"
        );
        for command in &controls.commands {
            ensure!(
                (command.name.starts_with('/')
                    || (matches!(command.name.as_str(), "!" | "!!")
                        && command.source == "builtin"
                        && controls
                            .features
                            .iter()
                            .any(|feature| feature.name == "shell_escapes" && feature.available)))
                    && command.name.len() <= 256
                    && !command.name.contains(char::is_whitespace)
                    && !command.name.contains(char::is_control)
                    && command.args.len() <= 4096
                    && command.description.len() <= 8192,
                "Invalid engine command catalog"
            );
            ensure!(
                command
                    .requirement
                    .as_ref()
                    .is_none_or(|requirement| requirement.len() <= 512
                        && !requirement.chars().any(char::is_control)),
                "Invalid command requirement"
            );
            ensure!(
                command.parameters.as_ref().is_none_or(|parameters| {
                    command.source == "template"
                        && parameters.len() <= 32
                        && parameters.iter().all(|name| {
                            !name.is_empty()
                                && name.len() <= 64
                                && name.bytes().all(|c| {
                                    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
                                })
                        })
                        && parameters
                            .iter()
                            .enumerate()
                            .all(|(index, name)| !parameters[..index].contains(name))
                }),
                "Invalid template parameters"
            );
        }
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Worker was replaced");
        session.controls = Some(controls);
        self.save_session(&session)
    }

    /// Retains a provider approval until its revision is resolved.
    ///
    /// # Errors
    /// Returns stale-worker or database errors.
    pub fn approval(
        &self,
        id: &str,
        generation: u64,
        mut approval: crate::model::PendingApproval,
    ) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Worker was replaced");
        let transaction = self.db.unchecked_transaction()?;
        if approval.requirement["kind"] == "plank_editor" {
            ensure!(
                approval.document.is_none(),
                "Document metadata is daemon-owned"
            );
            self.stage_document(id, &mut approval)?;
        }
        session.approvals.retain(|a| a.id != approval.id);
        session.approvals.push(approval);
        self.save_session(&session)?;
        transaction.commit()?;
        Ok(())
    }

    /// Removes a provider-resolved approval from the current session.
    ///
    /// # Errors
    /// Returns stale-worker or database errors.
    pub fn resolve_approval(&self, id: &str, generation: u64, approval_id: &str) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Worker was replaced");
        session.approvals.retain(|a| a.id != approval_id);
        self.save_session(&session)
    }

    /// Allows an explicit later prompt to resume a saved provider conversation.
    ///
    /// # Errors
    /// Returns busy, archived, unsupported-resume, or database errors.
    pub fn resume(&self, id: &str) -> Result<()> {
        let session = self.session(id)?;
        ensure!(
            !session.state.busy() && !session.archived,
            "Stop and restore this session before resuming"
        );
        ensure!(
            matches!(session.backend, Backend::Agent { .. })
                && session.resumable
                && session.agent_session.is_some(),
            "This runner has no saved context to resume"
        );
        self.invalidate(id, RunState::Idle, false)?;
        self.append(id, "system", "Saved context selected. The interrupted prompt will not be replayed; send a new message to continue.")
    }

    /// Durably accepts a prompt and rejects conflicting retry identifiers.
    ///
    /// Returns false when the identical request was already accepted.
    ///
    /// # Errors
    /// Returns errors for busy sessions, ambiguous recovery, or conflicting requests.
    pub fn accept_prompt(&mut self, id: &str, request_id: &str, text: &str) -> Result<bool> {
        ensure!(!text.trim().is_empty(), "Prompt is empty");
        ensure!(text.len() <= 64 * 1024, "Prompt exceeds 64 KiB");
        ensure!(!request_id.is_empty(), "Request identifier is required");
        let existing: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT session_id, text FROM requests WHERE id=?1",
                [request_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((old_id, old_text)) = existing {
            ensure!(
                old_id == id && old_text == text,
                "Request identifier was reused with different content"
            );
            return Ok(false);
        }
        let mut session = self.session(id)?;
        ensure!(
            !session.archived,
            "Restore this archived session before sending a prompt"
        );
        ensure!(!session.state.busy(), "This session is already running");
        ensure!(
            !session.recovery_required,
            "Engine context was lost; start a fresh context before continuing"
        );
        session.state = RunState::Starting;
        session.attention = false;
        session.started_at = Some(now());
        session.completed_at = None;
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT INTO requests VALUES(?1, ?2, ?3)",
            params![request_id, id, text],
        )?;
        tx.execute(
            "INSERT INTO entries(session_id,role,text) VALUES(?1,'user',?2)",
            params![id, text],
        )?;
        tx.execute(
            "UPDATE sessions SET data=?2 WHERE id=?1",
            params![id, serde_json::to_string(&session)?],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Records an ephemeral Plank question without changing the main turn.
    ///
    /// # Errors
    /// Returns validation, recovery, or database errors.
    pub fn accept_aside(&self, id: &str, request_id: &str, text: &str) -> Result<bool> {
        ensure!(
            crate::commands::aside_question(text).is_some(),
            "A side question is required"
        );
        ensure!(text.len() <= 64 * 1024, "Side question exceeds 64 KiB");
        ensure!(!request_id.is_empty(), "Request identifier is required");
        if self.aside_recorded(id, request_id, text)? {
            return Ok(false);
        }
        let session = self.session(id)?;
        ensure!(
            session.can_receive_asides(),
            "This Plank engine does not advertise independent side-question input"
        );
        ensure!(
            !session.archived && !session.recovery_required,
            "Restore the session's context before asking a side question"
        );
        self.db.execute(
            "INSERT INTO requests VALUES(?1,?2,?3)",
            params![request_id, id, text],
        )?;
        Ok(true)
    }

    /// Records an independent setting change without altering the main turn.
    ///
    /// # Errors
    /// Returns validation, recovery, or database errors.
    pub(crate) fn accept_live_control(
        &self,
        id: &str,
        request_id: &str,
        text: &str,
    ) -> Result<bool> {
        let control = crate::livecontrol::LiveControl::parse(text)
            .context("An explicit live control is required")?;
        ensure!(
            text.len() <= 64 * 1024 && !request_id.is_empty(),
            "Invalid live control request"
        );
        if self.aside_recorded(id, request_id, text)? {
            return Ok(false);
        }
        let session = self.session(id)?;
        ensure!(
            session.can_receive_live_control(control),
            "This engine does not advertise independent setting input"
        );
        ensure!(
            !session.archived && !session.recovery_required,
            "Restore the session before changing settings"
        );
        self.db.execute(
            "INSERT INTO requests VALUES(?1,?2,?3)",
            params![request_id, id, text],
        )?;
        Ok(true)
    }

    pub(crate) fn aside_recorded(&self, id: &str, request_id: &str, text: &str) -> Result<bool> {
        let existing: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT session_id,text FROM requests WHERE id=?1",
                [request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((old_id, old_text)) = existing {
            ensure!(
                old_id == id && old_text == text,
                "Request identifier was reused with different content"
            );
            return Ok(true);
        }
        Ok(false)
    }

    /// Appends one transcript chunk with a monotonic sequence.
    ///
    /// # Errors
    /// Returns database or unknown-session errors.
    pub fn append(&self, id: &str, role: &str, text: &str) -> Result<()> {
        self.session(id)?;
        self.db.execute(
            "INSERT INTO entries(session_id,role,text) VALUES(?1,?2,?3)",
            params![id, role, text],
        )?;
        Ok(())
    }

    /// Binds replay IDs to an authenticated server instance for this worker.
    ///
    /// Missing identities retain legacy replay behavior. A changed identity retires
    /// seen IDs and host-owned controls/dialogs while preserving transcript and receipts.
    ///
    /// # Errors
    /// Returns identity, backend, generation, database, or decoding errors.
    pub fn bind_remote_instance(
        &mut self,
        id: &str,
        generation: u64,
        instance: Option<&str>,
    ) -> Result<bool> {
        if let Some(instance) = instance {
            ensure!(
                !instance.is_empty()
                    && instance.len() <= 128
                    && instance
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')),
                "Invalid remote server instance identity"
            );
        }
        let mut session = self.session(id)?;
        ensure!(
            matches!(session.backend, Backend::Remote { .. }),
            "Only remote sessions have server instances"
        );
        ensure!(
            session.generation == generation,
            "Remote worker was replaced before authentication completed"
        );
        let prior: Option<String> = self
            .db
            .query_row(
                "SELECT instance FROM remote_hosts WHERE session_id=?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        let next = instance.unwrap_or_default();
        let seen: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM remote_seen WHERE session_id=?1)",
            [id],
            |row| row.get(0),
        )?;
        let changed = prior.as_deref().map_or_else(
            || {
                instance.is_some()
                    && (seen || session.controls.is_some() || !session.approvals.is_empty())
            },
            |prior| prior != next,
        );
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO remote_hosts(session_id,instance) VALUES(?1,?2) ON CONFLICT(session_id) DO UPDATE SET instance=excluded.instance", params![id,next])?;
        if changed {
            tx.execute("DELETE FROM remote_seen WHERE session_id=?1", [id])?;
            session.controls = None;
            session.approvals.clear();
            session.progress = None;
            session.attention = true;
            tx.execute(
                "UPDATE sessions SET data=?2 WHERE id=?1",
                params![id, serde_json::to_string(&session)?],
            )?;
            tx.execute("INSERT INTO entries(session_id,role,text) VALUES(?1,'system',?2)", params![id,"Remote Plank server instance changed; earlier entries are retained for reference."])?;
        }
        tx.commit()?;
        Ok(changed)
    }

    /// Records an ephemeral remote event without creating a transcript entry.
    pub(crate) fn record_remote_event(
        &self,
        id: &str,
        generation: u64,
        sequence: i64,
    ) -> Result<bool> {
        if sequence < 0 || self.session(id)?.generation != generation {
            return Ok(false);
        }
        Ok(self.db.execute(
            "INSERT OR IGNORE INTO remote_seen VALUES(?1,?2)",
            params![id, sequence],
        )? != 0)
    }

    /// Appends a remote chunk once, including reconnect snapshot replay.
    ///
    /// # Errors
    /// Returns database errors.
    pub fn append_remote(&mut self, id: &str, sequence: i64, role: &str, text: &str) -> Result<()> {
        let tx = self.db.transaction()?;
        if tx.execute(
            "INSERT OR IGNORE INTO remote_seen VALUES(?1,?2)",
            params![id, sequence],
        )? > 0
        {
            tx.execute(
                "INSERT INTO entries(session_id,role,text) VALUES(?1,?2,?3)",
                params![id, role, text],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Retires conversation-scoped controls once for a live engine reset.
    ///
    /// # Errors
    /// Returns storage or serialization errors; stale and replayed resets return false.
    pub(crate) fn conversation_replaced(
        &mut self,
        id: &str,
        generation: u64,
        sequence: Option<i64>,
    ) -> Result<bool> {
        let mut session = self.session(id)?;
        if session.generation != generation {
            return Ok(false);
        }
        let tx = self.db.transaction()?;
        if let Some(sequence) = sequence
            && tx.execute(
                "INSERT OR IGNORE INTO remote_seen VALUES(?1,?2)",
                params![id, sequence],
            )? == 0
        {
            return Ok(false);
        }
        session.approvals.clear();
        session.progress = None;
        session.agent_session = None;
        session.resumable = false;
        if let Some(controls) = &mut session.controls {
            controls.suggestion = None;
        }
        tx.execute(
            "UPDATE sessions SET data=?2 WHERE id=?1",
            params![id, serde_json::to_string(&session)?],
        )?;
        tx.execute(
            "INSERT INTO entries(session_id,role,text) VALUES(?1,'context_boundary',?2)",
            params![
                id,
                "Plank replaced its conversation; earlier messages are retained for reference."
            ],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Marks surviving engine contexts as lost after a daemon restart.
    ///
    /// # Errors
    /// Returns database errors.
    pub fn recover(&self) -> Result<()> {
        for session in self.snapshot(None, 0)?.sessions {
            if session.state.busy()
                || matches!(session.backend, Backend::Local { .. })
                || (matches!(session.backend, Backend::Agent { .. })
                    && session.agent_session.is_some()
                    && !session.resumable)
            {
                self.invalidate(&session.id, RunState::Interrupted, true)?;
                self.append(&session.id, "system", "Daemon restarted. Execution was not replayed. Start a fresh context to continue.")?;
            }
        }
        Ok(())
    }

    /// Preserves the transcript while explicitly starting a fresh engine context.
    ///
    /// # Errors
    /// Returns database errors.
    pub fn reset(&self, id: &str) -> Result<()> {
        self.invalidate(id, RunState::Idle, false)?;
        let mut session = self.session(id)?;
        session.agent_session = None;
        session.resumable = false;
        self.save_session(&session)?;
        self.db
            .execute("DELETE FROM remote_seen WHERE session_id=?1", [id])?;
        self.append(
            id,
            "context_boundary",
            "A fresh engine context starts here; earlier messages are retained for reference.",
        )
    }

    /// Invalidates previous worker writes before changing session state.
    ///
    /// # Errors
    /// Returns unknown-session, generation-overflow, or database errors.
    pub fn invalidate(&self, id: &str, state: RunState, recovery_required: bool) -> Result<()> {
        let mut session = self.session(id)?;
        session.generation = session
            .generation
            .checked_add(1)
            .context("Session generation exhausted")?;
        session.state = state;
        session.recovery_required = recovery_required;
        session.approvals.clear();
        session.controls = None;
        session.progress = None;
        self.save_session(&session)
    }

    /// Reads metadata and a bounded transcript page for one selected session.
    ///
    /// # Errors
    /// Returns database or serialization errors.
    pub fn snapshot(&self, session_id: Option<&str>, before: i64) -> Result<Snapshot> {
        self.snapshot_pages(session_id, before, 0)
    }

    /// Reads transcript and aside pages with independent cursors.
    ///
    /// # Errors
    /// Returns database or decoding errors.
    pub fn snapshot_pages(
        &self,
        session_id: Option<&str>,
        before: i64,
        aside_before: i64,
    ) -> Result<Snapshot> {
        let mut snapshot = Snapshot::default();
        for (table, workspaces) in [("workspaces", true), ("sessions", false)] {
            let mut statement = self
                .db
                .prepare(&format!("SELECT data FROM {table} ORDER BY rowid"))?;
            let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                let data = row?;
                if workspaces {
                    snapshot.workspaces.push(serde_json::from_str(&data)?);
                } else {
                    snapshot.sessions.push(serde_json::from_str(&data)?);
                }
            }
        }
        if let Some(id) = session_id {
            for (side, cursor) in [(false, before), (true, aside_before)] {
                let sql = if side {
                    "SELECT sequence,session_id,role,text FROM entries WHERE session_id=?1 AND role IN ('aside','aside_thinking','aside_tool','aside_notice','aside_error') AND (?2=0 OR sequence<?2) ORDER BY sequence DESC LIMIT 250"
                } else {
                    "SELECT sequence,session_id,role,text FROM entries WHERE session_id=?1 AND role NOT IN ('aside','aside_thinking','aside_tool','aside_notice','aside_error') AND (?2=0 OR sequence<?2)
                 ORDER BY sequence DESC LIMIT 250"
                };
                let mut statement = self.db.prepare(sql)?;
                let rows = statement.query_map(params![id, cursor], |r| {
                    Ok(Entry {
                        sequence: r.get(0)?,
                        session_id: r.get(1)?,
                        role: r.get(2)?,
                        text: r.get(3)?,
                    })
                })?;
                for row in rows {
                    if side {
                        snapshot.asides.push(row?);
                    } else {
                        snapshot.entries.push(row?);
                    }
                }
            }
            snapshot.entries.reverse();
            snapshot.asides.reverse();
        }
        snapshot.sessions.sort_by_key(|session| session.position);
        for session in &mut snapshot.sessions {
            if let Some(progress) = &session.progress {
                session.summary = progress
                    .current
                    .saturating_mul(100)
                    .checked_div(progress.total)
                    .map_or_else(
                        || progress.label.clone(),
                        |percent| format!("{} · {percent}%", progress.label),
                    );
                continue;
            }
            session.summary = self.db.query_row(
                "SELECT substr(text,1,200) FROM entries WHERE session_id=?1 AND role IN ('assistant','user') ORDER BY sequence DESC LIMIT 1",
                [&session.id], |row| row.get::<_, String>(0),
            ).optional()?.unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").chars().take(140).collect();
        }
        Ok(snapshot)
    }

    /// Marks a newly opened interactive surface as needing attention.
    ///
    /// # Errors
    /// Returns storage or unknown-session errors.
    pub fn request_attention(&self, id: &str) -> Result<()> {
        let mut session = self.session(id)?;
        session.attention = true;
        self.save_session(&session)
    }

    /// Clears completion attention without dismissing pending approvals.
    ///
    /// # Errors
    /// Returns storage or unknown-session errors.
    pub fn acknowledge(&self, id: &str) -> Result<()> {
        let mut session = self.session(id)?;
        if session.attention {
            session.attention = false;
            self.save_session(&session)?;
        }
        Ok(())
    }

    /// Updates bounded engine progress only for the current worker generation.
    ///
    /// # Errors
    /// Returns validation, generation, or persistence errors.
    pub fn engine_progress(
        &self,
        id: &str,
        generation: u64,
        progress: crate::model::EngineProgress,
    ) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Stale engine progress");
        ensure!(
            progress.label.len() <= 256
                && !progress.label.chars().any(char::is_control)
                && (progress.total == 0 || progress.current <= progress.total),
            "Invalid engine progress"
        );
        session.progress = Some(progress);
        self.save_session(&session)
    }

    /// Marks an expected engine restart while retaining accepted input and history.
    ///
    /// # Errors
    /// Returns generation or persistence errors.
    pub fn engine_restarting(&self, id: &str, generation: u64) -> Result<()> {
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Stale engine restart");
        session.controls = None;
        session.approvals.clear();
        session.progress = Some(crate::model::EngineProgress {
            label: "Restarting engine".into(),
            current: 0,
            total: 0,
        });
        self.save_session(&session)
    }

    /// Persists current launch arguments so a fresh context uses the selected engine.
    ///
    /// # Errors
    /// Returns validation, generation, backend, or persistence errors.
    pub fn engine_launch(&self, id: &str, generation: u64, args: Vec<String>) -> Result<()> {
        ensure!(
            args.len() <= 128
                && args.iter().all(|arg| arg.len() < 16 * 1024
                    && !matches!(
                        arg.as_str(),
                        "--ui"
                            | "-p"
                            | "--prompt"
                            | "--pick-engine"
                            | "--pick-engine-resume"
                            | "/resume"
                    )),
            "Invalid persistent engine arguments"
        );
        let mut session = self.session(id)?;
        ensure!(session.generation == generation, "Stale engine launch");
        let Backend::Local { args: current, .. } = &mut session.backend else {
            anyhow::bail!("Launch settings require a local Plank session");
        };
        *current = args;
        session.progress = None;
        self.save_session(&session)
    }

    /// Rejects unsupported database versions before modifying application data.
    ///
    /// # Errors
    /// Returns an error if a future schema is encountered.
    pub fn check_version(&self) -> Result<()> {
        let version: i64 = self.db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 3 {
            bail!("Database was created by a newer nplank version");
        }
        self.db.pragma_update(None, "user_version", 3)?;
        Ok(())
    }
}
