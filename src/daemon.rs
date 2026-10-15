//! Authenticated loopback API with independent execution and durable state.

// Rust guideline compliant 2026-02-21
use std::{
    collections::HashMap,
    fs::OpenOptions,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use fs2::FileExt;
use serde::Deserialize;
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    model::{Backend, Command, Receipt, RunState, Snapshot},
    store::Store,
    workers,
};

/// Default loopback address shared by the desktop client and daemon.
pub const DEFAULT_ADDRESS: &str = "127.0.0.1:47831";

/// Persistent daemon with one independent task per attached engine.
#[derive(Debug)]
pub struct Daemon {
    pub(crate) notifications: Mutex<crate::notifications::Feed>,
    pub(crate) views: Mutex<crate::focus::Views>,
    pub(crate) artifacts: Mutex<crate::artifacts::Artifacts>,
    pub(crate) telemetry: Mutex<HashMap<String, (u64, crate::model::EngineTelemetry)>>,
    pub(crate) processes: Mutex<HashMap<String, (u64, u32)>>,
    pub(crate) contexts: Mutex<HashMap<String, (u64, crate::model::SessionContext)>>,
    pub(crate) store: Mutex<Store>,
    commands: Mutex<()>,
    workers: Mutex<HashMap<String, Worker>>,
    token: String,
}

#[derive(Debug)]
struct Worker {
    input: mpsc::Sender<workers::Input>,
    task: JoinHandle<()>,
}

impl Daemon {
    /// Opens and recovers the persistent workspace state.
    ///
    /// # Errors
    /// Returns database and recovery errors.
    pub fn open(path: &Path, token: String) -> Result<Arc<Self>> {
        ensure!(!token.is_empty(), "Authentication token cannot be empty");
        let store = Store::open(path)?;
        store.check_version()?;
        store.recover()?;
        let staging = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let daemon = Arc::new(Self {
            notifications: Mutex::default(),
            views: Mutex::default(),
            artifacts: Mutex::new(crate::artifacts::Artifacts::new(staging)?),
            telemetry: Mutex::new(HashMap::new()),
            processes: Mutex::new(HashMap::new()),
            contexts: Mutex::new(HashMap::new()),
            store: Mutex::new(store),
            commands: Mutex::new(()),
            workers: Mutex::new(HashMap::new()),
            token,
        });
        crate::metadata::start(&daemon);
        Ok(daemon)
    }

    /// Reads a bounded view without transferring engine ownership.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn snapshot(&self, id: Option<&str>, before: i64) -> Result<Snapshot> {
        self.snapshot_pages(id, before, 0)
    }

    /// Reads independent transcript and aside pages without changing execution.
    ///
    /// # Errors
    /// Returns storage errors.
    pub fn snapshot_pages(
        &self,
        id: Option<&str>,
        before: i64,
        aside_before: i64,
    ) -> Result<Snapshot> {
        let mut snapshot =
            self.store
                .lock()
                .expect("store poisoned")
                .snapshot_pages(id, before, aside_before)?;
        snapshot.providers = crate::agents::providers();
        snapshot.protocol_version = crate::model::PROTOCOL_VERSION;
        snapshot.server_time_ms = crate::notifications::now_ms();
        snapshot.notifications = self
            .notifications
            .lock()
            .expect("notifications poisoned")
            .snapshot(&snapshot.sessions, snapshot.server_time_ms);
        let telemetry = self.telemetry.lock().expect("telemetry poisoned");
        for session in &snapshot.sessions {
            if !session.recovery_required
                && !matches!(session.state, RunState::Failed | RunState::Interrupted)
                && let Some((generation, counters)) = telemetry.get(&session.id)
                && *generation == session.generation
            {
                let mut counters = counters.clone();
                if id != Some(session.id.as_str()) {
                    counters.buddy = None;
                }
                snapshot.telemetry.insert(session.id.clone(), counters);
            }
        }
        drop(telemetry);
        // Verified files outlive live counters and socket attachments. Reconstruct
        // their small descriptors without resurrecting live engine status.
        let artifacts = self.artifacts.lock().expect("artifacts poisoned");
        for session in &snapshot.sessions {
            let files = artifacts.metadata(&session.id, session.generation);
            if !files.is_empty() {
                let counters = snapshot.telemetry.entry(session.id.clone()).or_default();
                counters
                    .replace_artifacts(files, artifacts.retired(&session.id, session.generation));
            }
        }
        drop(artifacts);
        let contexts = self.contexts.lock().expect("contexts poisoned");
        for session in &snapshot.sessions {
            if let Some((generation, context)) = contexts.get(&session.id)
                && *generation == session.generation
            {
                snapshot
                    .contexts
                    .insert(session.id.clone(), context.clone());
            }
        }
        Ok(snapshot)
    }

    fn detach_worker(&self, id: &str) {
        if let Some(worker) = self.workers.lock().expect("workers poisoned").remove(id) {
            worker.task.abort();
        }
    }

    /// Retires an intentionally closed local engine before accepting new input.
    pub(crate) fn local_engine_exited(&self, session: &crate::model::Session) -> Result<()> {
        let _command = self.commands.lock().expect("commands poisoned");
        let mut active = self.workers.lock().expect("workers poisoned");
        let store = self.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation != session.generation {
            return Ok(());
        }
        store.reset(&session.id)?;
        let generation = store.session(&session.id)?.generation;
        active.remove(&session.id);
        self.telemetry
            .lock()
            .expect("telemetry poisoned")
            .remove(&session.id);
        self.contexts
            .lock()
            .expect("contexts poisoned")
            .remove(&session.id);
        self.artifacts
            .lock()
            .expect("artifacts poisoned")
            .retain_generation(&session.id, generation);
        Ok(())
    }

    /// Applies a command, persisting prompts before execution begins.
    ///
    /// # Errors
    /// Returns validation, storage, and dispatch errors.
    pub fn execute(self: &Arc<Self>, command: Command) -> Result<Receipt> {
        let _command = self.commands.lock().expect("commands poisoned");
        let id = match command {
            Command::ViewFocus {
                client_id,
                session_id,
                generation,
            } => {
                if let Some(id) = &session_id {
                    let session = self.store.lock().expect("store poisoned").session(id)?;
                    ensure!(
                        session.generation == generation
                            && !session.archived
                            && !session.recovery_required
                            && matches!(session.backend, Backend::Local { .. }),
                        "Focus belongs to an inactive local session"
                    );
                }
                self.views.lock().expect("view focus poisoned").update(
                    &client_id,
                    session_id.as_deref().map(|id| (id, generation)),
                    std::time::Instant::now(),
                )?;
                session_id.unwrap_or_default()
            }
            Command::StopJobOutput {
                session_id,
                generation,
                observer,
            } => {
                ensure!(
                    crate::joboutput::valid_observer(&observer),
                    "Invalid output observer"
                );
                let session = self
                    .store
                    .lock()
                    .expect("store poisoned")
                    .session(&session_id)?;
                ensure!(
                    session.generation == generation
                        && !session.archived
                        && !session.recovery_required,
                    "Output belongs to an old engine generation"
                );
                ensure!(
                    matches!(
                        session.backend,
                        Backend::Local { .. } | Backend::Remote { .. }
                    ),
                    "This backend has no output control channel"
                );
                ensure!(
                    session.controls.as_ref().is_some_and(|controls| controls
                        .features
                        .iter()
                        .any(|feature| feature.name == "job_output_cancel" && feature.available)),
                    "Host requires queued /jobs unfollow"
                );
                self.ensure_remote_control(&session)?;
                ensure!(
                    workers::job_observer_current(self, &session, &observer)?,
                    "Output observer is no longer live"
                );
                let workers = self.workers.lock().expect("workers poisoned");
                workers
                    .get(&session_id)
                    .filter(|worker| !worker.task.is_finished())
                    .context("Output engine disconnected")?
                    .input
                    .try_send(workers::Input::StopJobOutput(observer))
                    .context("Output control queue is full; retry")?;
                session_id
            }
            Command::FrameControl {
                session_id,
                generation,
                request,
            } => {
                ensure!(request.is_valid(), "Invalid frame control");
                let session = self
                    .store
                    .lock()
                    .expect("store poisoned")
                    .session(&session_id)?;
                ensure!(
                    session.generation == generation
                        && !session.archived
                        && !session.recovery_required,
                    "Frame belongs to an old engine generation"
                );
                ensure!(
                    matches!(
                        session.backend,
                        Backend::Local { .. } | Backend::Remote { .. }
                    ),
                    "This host does not support graphical frame input yet"
                );
                self.ensure_remote_control(&session)?;
                ensure!(
                    self.telemetry
                        .lock()
                        .expect("telemetry poisoned")
                        .get(&session_id)
                        .filter(|(owner, _)| *owner == generation)
                        .and_then(|(_, telemetry)| telemetry.frame.as_ref())
                        .is_some_and(|frame| frame.id == request.id),
                    "Frame is no longer open"
                );
                let workers = self.workers.lock().expect("workers poisoned");
                let worker = workers
                    .get(&session_id)
                    .filter(|worker| !worker.task.is_finished())
                    .context("Frame engine disconnected")?;
                worker
                    .input
                    .try_send(workers::Input::FrameControl(request))
                    .context("Frame input queue is full; retry")?;
                drop(workers);
                self.store
                    .lock()
                    .expect("store poisoned")
                    .acknowledge(&session_id)?;
                session_id
            }
            Command::ConnectRemote { session_id } => {
                let session = self
                    .store
                    .lock()
                    .expect("store poisoned")
                    .session(&session_id)?;
                ensure!(
                    matches!(session.backend, Backend::Remote { .. }),
                    "Only remote sessions can attach as observers"
                );
                ensure!(
                    !session.state.busy() && !session.recovery_required,
                    "Recover or finish this session before attaching"
                );
                let connected = self
                    .workers
                    .lock()
                    .expect("workers poisoned")
                    .get(&session_id)
                    .is_some_and(|worker| !worker.task.is_finished());
                if !connected {
                    self.telemetry
                        .lock()
                        .expect("telemetry poisoned")
                        .remove(&session_id);
                    self.store.lock().expect("store poisoned").set_state(
                        &session_id,
                        RunState::Starting,
                        false,
                    )?;
                    self.dispatch_input(&session_id, workers::Input::Observe)?;
                }
                session_id
            }
            Command::RemoteControl {
                session_id,
                request,
            } => {
                let session = self
                    .store
                    .lock()
                    .expect("store poisoned")
                    .session(&session_id)?;
                ensure!(
                    matches!(session.backend, Backend::Remote { .. }) && !session.recovery_required,
                    "Remote control requires an attached remote session"
                );
                let waiting = self
                    .telemetry
                    .lock()
                    .expect("telemetry poisoned")
                    .get(&session_id)
                    .filter(|(generation, _)| *generation == session.generation)
                    .and_then(|(_, telemetry)| telemetry.remote_access.as_ref())
                    .is_some_and(|access| access.waiting_input);
                let frame_open = self
                    .telemetry
                    .lock()
                    .expect("telemetry poisoned")
                    .get(&session_id)
                    .filter(|(generation, _)| *generation == session.generation)
                    .is_some_and(|(_, telemetry)| telemetry.frame.is_some());
                ensure!(
                    !session.state.busy() || frame_open || (request && waiting),
                    "Finish or cancel input before changing control"
                );
                self.workers
                    .lock()
                    .expect("workers poisoned")
                    .get(&session_id)
                    .context("Connect as observer first")?
                    .input
                    .try_send(workers::Input::RemoteControl(request))
                    .context("Remote connection cannot accept a control request")?;
                session_id
            }
            Command::AcknowledgeSession { session_id } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .acknowledge(&session_id)?;
                session_id
            }
            Command::RenameWorkspace { workspace_id, name } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .rename_workspace(&workspace_id, &name)?;
                workspace_id
            }
            Command::ArchiveSession {
                session_id,
                archived,
            } => {
                let store = self.store.lock().expect("store poisoned");
                store.archive(&session_id, archived)?;
                if archived {
                    let session = store.session(&session_id)?;
                    self.detach_worker(&session_id);
                    let lost = matches!(session.backend, Backend::Local { .. })
                        || (matches!(session.backend, Backend::Agent { .. })
                            && !session.resumable
                            && session.agent_session.is_some());
                    store.invalidate(
                        &session_id,
                        if lost {
                            RunState::Interrupted
                        } else {
                            RunState::Idle
                        },
                        lost,
                    )?;
                }
                session_id
            }
            Command::ReorderSession {
                session_id,
                position,
            } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .reorder(&session_id, position)?;
                session_id
            }
            Command::Resume { session_id } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .resume(&session_id)?;
                self.detach_worker(&session_id);
                session_id
            }
            Command::DecideApproval {
                session_id,
                approval_id,
                choice_id,
                requirement,
            } => {
                let store = self.store.lock().expect("store poisoned");
                let session = store.session(&session_id)?;
                self.ensure_remote_control(&session)?;
                let approval = session
                    .approvals
                    .iter()
                    .find(|a| a.id == approval_id)
                    .context("Approval is no longer pending")?;
                ensure!(
                    approval.requirement["kind"] != "plank_editor",
                    "Use the document save or cancel controls"
                );
                ensure!(
                    approval.requirement == requirement,
                    "Approval changed; review the current request before deciding"
                );
                ensure!(
                    approval.accepts_choice(&choice_id),
                    "Invalid approval choice"
                );
                self.workers
                    .lock()
                    .expect("workers poisoned")
                    .get(&session_id)
                    .context("Agent is no longer connected")?
                    .input
                    .try_send(workers::Input::Approval {
                        approval: approval.clone(),
                        choice: choice_id,
                    })
                    .context("Agent cannot accept a decision yet")?;
                session_id
            }
            Command::EditDocument {
                session_id,
                approval_id,
                requirement,
                text,
            } => {
                ensure!(
                    text.as_ref()
                        .is_none_or(|text| text.len() <= crate::documenttransfer::DOCUMENT_BYTES),
                    "Document exceeds the editing limit"
                );
                let store = self.store.lock().expect("store poisoned");
                let session = store.session(&session_id)?;
                self.ensure_remote_control(&session)?;
                ensure!(
                    text.as_ref().is_none_or(|text| text.len()
                        <= crate::commands::editor_save_limit(session.controls.as_ref())),
                    "This engine does not support larger document Saves"
                );
                let editor = session
                    .approvals
                    .iter()
                    .find(|approval| approval.id == approval_id)
                    .context("Editor is no longer pending")?;
                ensure!(
                    editor.requirement["kind"] == "plank_editor"
                        && editor.requirement == requirement,
                    "Editor changed; review the current document before saving"
                );
                self.workers
                    .lock()
                    .expect("workers poisoned")
                    .get(&session_id)
                    .context("Engine is no longer connected")?
                    .input
                    .try_send(workers::Input::Approval {
                        approval: editor.clone(),
                        choice: serde_json::to_string(&text)?,
                    })
                    .context("Engine cannot accept the document yet")?;
                session_id
            }
            Command::CreateWorkspace { name, directory } => self
                .store
                .lock()
                .expect("store poisoned")
                .create_workspace(&name, &directory)?,
            Command::CreateSession {
                workspace_id,
                name,
                backend,
            } => self.store.lock().expect("store poisoned").create_session(
                &workspace_id,
                &name,
                backend,
            )?,
            Command::RenameSession { session_id, name } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .rename_session(&session_id, &name)?;
                session_id
            }
            Command::MoveSession {
                session_id,
                workspace_id,
            } => {
                let store = self.store.lock().expect("store poisoned");
                store.move_session(&session_id, &workspace_id)?;
                self.detach_worker(&session_id);
                store.reset(&session_id)?;
                session_id
            }
            Command::Reset { session_id } => {
                self.store
                    .lock()
                    .expect("store poisoned")
                    .session(&session_id)?;
                self.detach_worker(&session_id);
                self.store
                    .lock()
                    .expect("store poisoned")
                    .reset(&session_id)?;
                session_id
            }
            Command::Stop { session_id } => {
                let store = self.store.lock().expect("store poisoned");
                let session = store.session(&session_id)?;
                if matches!(session.backend, Backend::Remote { .. }) {
                    if let Some(worker) = self
                        .workers
                        .lock()
                        .expect("workers poisoned")
                        .get(&session_id)
                    {
                        worker
                            .input
                            .try_send(workers::Input::Stop)
                            .context("Remote interrupt could not be queued")?;
                    }
                } else {
                    self.detach_worker(&session_id);
                }
                let context_lost = !matches!(session.backend, Backend::Demo);
                store.invalidate(&session_id, RunState::Interrupted, context_lost)?;
                store.append(
                    &session_id,
                    "system",
                    "Stop requested. Previously executed tools were not undone.",
                )?;
                session_id
            }
            Command::Prompt {
                session_id,
                request_id,
                text,
            } => {
                let live_control =
                    crate::livecontrol::LiveControl::parse(&text).filter(|control| {
                        self.store
                            .lock()
                            .expect("store poisoned")
                            .session(&session_id)
                            .is_ok_and(|session| session.can_receive_live_control(*control))
                    });
                if let Some(control) = live_control {
                    let session = self
                        .store
                        .lock()
                        .expect("store poisoned")
                        .session(&session_id)?;
                    self.ensure_remote_control(&session)?;
                    if self.store.lock().expect("store poisoned").aside_recorded(
                        &session_id,
                        &request_id,
                        &text,
                    )? {
                        return Ok(Receipt { id: session_id });
                    }
                    let workers = self.workers.lock().expect("workers poisoned");
                    let worker = workers
                        .get(&session_id)
                        .filter(|worker| !worker.task.is_finished())
                        .context("Connect to the Plank engine before changing live settings")?;
                    let permit = worker
                        .input
                        .try_reserve()
                        .context("Plank cannot accept a live setting yet")?;
                    if self
                        .store
                        .lock()
                        .expect("store poisoned")
                        .accept_live_control(&session_id, &request_id, &text)?
                    {
                        permit.send(workers::Input::LiveControl(control));
                    }
                    return Ok(Receipt { id: session_id });
                }
                let independent_aside = crate::commands::aside_question(&text).filter(|_| {
                    self.store
                        .lock()
                        .expect("store poisoned")
                        .session(&session_id)
                        .is_ok_and(|session| session.can_receive_asides())
                });
                if let Some(question) = independent_aside {
                    if self.store.lock().expect("store poisoned").aside_recorded(
                        &session_id,
                        &request_id,
                        &text,
                    )? {
                        return Ok(Receipt { id: session_id });
                    }
                    let workers = self.workers.lock().expect("workers poisoned");
                    let worker = workers
                        .get(&session_id)
                        .filter(|worker| !worker.task.is_finished())
                        .context("Connect to the Plank engine before asking a side question")?;
                    // Reserve capacity before committing the idempotent receipt.
                    let permit = worker
                        .input
                        .try_reserve()
                        .context("Plank cannot accept a side question yet")?;
                    if self.store.lock().expect("store poisoned").accept_aside(
                        &session_id,
                        &request_id,
                        &text,
                    )? {
                        permit.send(workers::Input::Aside(question.into()));
                    }
                    return Ok(Receipt { id: session_id });
                }
                let accepted = self.store.lock().expect("store poisoned").accept_prompt(
                    &session_id,
                    &request_id,
                    &text,
                )?;
                if accepted && let Err(error) = self.dispatch(&session_id, text) {
                    self.store.lock().expect("store poisoned").set_state(
                        &session_id,
                        RunState::Failed,
                        true,
                    )?;
                    return Err(error);
                }
                session_id
            }
        };
        if let Ok(session) = self.store.lock().expect("store poisoned").session(&id) {
            self.artifacts
                .lock()
                .expect("artifacts poisoned")
                .retain_generation(&id, session.generation);
        }
        Ok(Receipt { id })
    }

    fn ensure_remote_control(&self, session: &crate::model::Session) -> Result<()> {
        if matches!(session.backend, Backend::Remote { .. }) {
            ensure!(
                self.telemetry
                    .lock()
                    .expect("telemetry poisoned")
                    .get(&session.id)
                    .filter(|(generation, _)| *generation == session.generation)
                    .and_then(|(_, telemetry)| telemetry.remote_access.as_ref())
                    .is_some_and(|access| access.controller),
                "Request remote control before answering or saving"
            );
        }
        Ok(())
    }

    fn dispatch(self: &Arc<Self>, id: &str, text: String) -> Result<()> {
        self.dispatch_input(id, workers::Input::Prompt(text))
    }

    fn dispatch_input(self: &Arc<Self>, id: &str, message: workers::Input) -> Result<()> {
        let mut active = self.workers.lock().expect("workers poisoned");
        if active
            .get(id)
            .is_some_and(|worker| worker.task.is_finished())
        {
            active.remove(id);
        }
        if !active.contains_key(id) {
            let store = self.store.lock().expect("store poisoned");
            let session = store.session(id)?;
            let workspace = store.workspace(&session.workspace_id)?;
            let (input, receiver) = mpsc::channel(1);
            let daemon = Arc::clone(self);
            let task = tokio::spawn(async move {
                if let Err(error) = workers::run(
                    Arc::clone(&daemon),
                    &session,
                    &workspace.directory,
                    receiver,
                )
                .await
                {
                    let store = daemon.store.lock().expect("store poisoned");
                    if store
                        .session(&session.id)
                        .is_ok_and(|current| current.generation == session.generation)
                    {
                        let _ = store.append(&session.id, "error", &error.to_string());
                        let _ = store.set_state(
                            &session.id,
                            RunState::Failed,
                            !matches!(session.backend, Backend::Demo),
                        );
                    }
                }
            });
            active.insert(id.into(), Worker { input, task });
        }
        active
            .get(id)
            .context("Worker was not created")?
            .input
            .try_send(message)
            .context("Worker cannot accept input")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_live_settings_require_control_before_recording_the_receipt() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED".into(),
                },
            )?;
            store.set_state(&id, RunState::Running, false)?;
            let session = store.session(&id)?;
            store.controls(&id, session.generation, serde_json::from_value(serde_json::json!({"version":1,"commands":[],"features":[{"name":"remote_live_controls","label":"Live settings","available":true,"detail":"Independent settings"}]}))?)?;
            store.session(&id)?
        };
        let (input, mut receiver) = mpsc::channel(1);
        let task = tokio::spawn(std::future::pending());
        daemon
            .workers
            .lock()
            .expect("workers poisoned")
            .insert(session.id.clone(), Worker { input, task });
        let command = || Command::Prompt {
            session_id: session.id.clone(),
            request_id: "remote-setting".into(),
            text: "/mc off".into(),
        };
        assert!(daemon.execute(command()).is_err());
        assert!(!daemon.store.lock().unwrap().aside_recorded(
            &session.id,
            "remote-setting",
            "/mc off"
        )?);
        let access = crate::model::RemoteAccess {
            client_id: Some(7),
            controller: true,
            requesting: false,
            waiting_input: false,
            reason: None,
        };
        daemon.telemetry.lock().unwrap().insert(
            session.id.clone(),
            (
                session.generation + 1,
                crate::model::EngineTelemetry {
                    remote_access: Some(access.clone()),
                    ..Default::default()
                },
            ),
        );
        assert!(
            daemon.execute(command()).is_err(),
            "Stale grants cannot authorize new settings"
        );
        daemon.telemetry.lock().unwrap().insert(
            session.id.clone(),
            (
                session.generation,
                crate::model::EngineTelemetry {
                    remote_access: Some(access),
                    ..Default::default()
                },
            ),
        );
        daemon.execute(command())?;
        assert!(matches!(
            receiver.try_recv()?,
            workers::Input::LiveControl(crate::livecontrol::LiveControl::MicroCompaction(false))
        ));
        daemon.execute(Command::Reset {
            session_id: session.id,
        })?;
        Ok(())
    }

    #[tokio::test]
    async fn live_settings_preserve_turns_and_retry_refused_requests() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Local", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                Backend::Local {
                    executable: "unused".into(),
                    args: vec![],
                },
            )?;
            store.set_state(&id, RunState::Running, false)?;
            store.append(&id, "user", "Main task")?;
            id
        };
        let command = |request: &str, text: &str| Command::Prompt {
            session_id: id.clone(),
            request_id: request.into(),
            text: text.into(),
        };
        // An older engine must not bypass ordinary busy-turn validation.
        assert!(daemon.execute(command("old", "/mc off")).is_err());
        let controls = serde_json::from_value(
            serde_json::json!({"version":1,"commands":[],"features":[{"name":"console_live_controls","label":"Live settings","available":true,"detail":"Independent settings"}]}),
        )?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let session = store.session(&id)?;
            store.controls(&id, session.generation, controls)?;
            store.session(&id)?
        };
        let mut remote = session.clone();
        remote.backend = Backend::Remote {
            url: "ws://unused".into(),
            token_env: "UNUSED".into(),
        };
        assert!(
            !remote.can_receive_live_controls(),
            "A local capability must not enable unsupported remote input"
        );
        let (input, mut receiver) = mpsc::channel(1);
        let task = tokio::spawn(std::future::pending());
        daemon
            .workers
            .lock()
            .expect("workers poisoned")
            .insert(id.clone(), Worker { input, task });
        let before = daemon.snapshot(Some(&id), 0)?;
        daemon.execute(command("mc", "/mc off"))?;
        daemon.execute(command("mc", "/mc off"))?;
        assert!(daemon.execute(command("mc", "/mc on")).is_err());
        assert!(daemon.execute(command("lg", "/loopguard off")).is_err());
        assert!(matches!(
            receiver.try_recv()?,
            workers::Input::LiveControl(crate::livecontrol::LiveControl::MicroCompaction(false))
        ));
        daemon.execute(command("lg", "/loopguard off"))?;
        assert!(matches!(
            receiver.try_recv()?,
            workers::Input::LiveControl(crate::livecontrol::LiveControl::LoopGuard(false))
        ));
        assert!(receiver.try_recv().is_err());
        assert!(!session.can_receive_live_control(crate::livecontrol::LiveControl::Hints(true)));
        assert!(daemon.execute(command("hints", "/hints on")).is_err());
        {
            let store = daemon.store.lock().expect("store poisoned");
            let mut controls = session.controls.clone().unwrap();
            controls.features.push(crate::commands::EngineFeature {
                name: "live_hints".into(),
                label: "Hints".into(),
                available: true,
                detail: "Mid-turn hints".into(),
            });
            store.controls(&id, session.generation, controls)?;
        }
        daemon.execute(command("hints", "/hints on"))?;
        daemon.execute(command("hints", "/hints on"))?;
        assert!(matches!(
            receiver.try_recv()?,
            workers::Input::LiveControl(crate::livecontrol::LiveControl::Hints(true))
        ));
        assert!(receiver.try_recv().is_err());
        let notification =
            crate::livecontrol::LiveControl::Notifications(crate::livecontrol::NotifyMode::Never);
        assert!(!daemon.snapshot(Some(&id), 0)?.sessions[0].can_receive_live_control(notification));
        assert!(daemon.execute(command("notify", "/notify never")).is_err());
        {
            let store = daemon.store.lock().expect("store poisoned");
            let mut controls = store.session(&id)?.controls.unwrap();
            controls.features.push(crate::commands::EngineFeature {
                name: "live_notify".into(),
                label: "Notify".into(),
                available: true,
                detail: "Independent policy".into(),
            });
            store.controls(&id, session.generation, controls)?;
        }
        daemon.execute(command("notify", "/notify never"))?;
        daemon.execute(command("notify", "/notify never"))?;
        assert!(
            matches!(receiver.try_recv()?, workers::Input::LiveControl(control) if control == notification)
        );
        assert!(receiver.try_recv().is_err());
        let after = daemon.snapshot(Some(&id), 0)?;
        assert_eq!(before.entries, after.entries);
        assert_eq!(after.sessions[0].state, RunState::Running);
        assert_eq!(after.sessions[0].generation, session.generation);
        daemon.execute(Command::Reset {
            session_id: id.clone(),
        })?;
        assert!(!daemon.snapshot(Some(&id), 0)?.sessions[0].can_receive_live_controls());
        assert!(receiver.try_recv().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn remote_asides_preserve_active_turns_deduplicate_and_retry_queue_refusals() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.set_state(&id, RunState::Running, false)?;
            store.append(&id, "user", "Main task")?;
            id
        };
        let (input, mut receiver) = mpsc::channel(1);
        let task = tokio::spawn(std::future::pending());
        daemon
            .workers
            .lock()
            .expect("workers poisoned")
            .insert(id.clone(), Worker { input, task });
        let before = daemon.snapshot(Some(&id), 0)?;
        let command = |request: &str, text: &str| Command::Prompt {
            session_id: id.clone(),
            request_id: request.into(),
            text: text.into(),
        };
        // No controller grant is needed: Plank allows observers to ask asides.
        daemon.execute(command("side-1", "/btw Why?"))?;
        daemon.execute(command("side-1", "/btw Why?"))?;
        assert!(
            daemon
                .execute(command("main-2", "Another main task"))
                .is_err()
        );
        assert!(
            daemon
                .execute(command("invalid-side", "/btwfoo Why?"))
                .is_err()
        );
        assert!(daemon.execute(command("side-1", "/btw Different")).is_err());
        // The queue is full; a refused question must remain retryable.
        assert!(daemon.execute(command("side-2", "/btw Next?")).is_err());
        assert!(matches!(receiver.try_recv()?, workers::Input::Aside(text) if text == "Why?"));
        daemon.execute(command("side-2", "/btw Next?"))?;
        assert!(matches!(receiver.try_recv()?, workers::Input::Aside(text) if text == "Next?"));
        assert!(receiver.try_recv().is_err());
        let after = daemon.snapshot(Some(&id), 0)?;
        assert_eq!(after.entries, before.entries);
        assert!(after.asides.is_empty());
        assert_eq!(after.sessions[0].state, RunState::Running);
        assert_eq!(after.sessions[0].generation, before.sessions[0].generation);
        daemon.execute(Command::Reset {
            session_id: id.clone(),
        })?;
        assert!(
            daemon
                .execute(command("side-3", "/btw After reset?"))
                .is_err()
        );
        assert!(receiver.try_recv().is_err());
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
struct SnapshotQuery {
    session: Option<String>,
    #[serde(default)]
    before: i64,
    #[serde(default)]
    aside_before: i64,
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error": self.1}))).into_response()
    }
}

fn authorize(daemon: &Daemon, headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    let supplied = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    if supplied != Some(daemon.token.as_str()) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Authentication required".into(),
        ));
    }
    Ok(())
}

async fn snapshot(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Query(query): Query<SnapshotQuery>,
) -> std::result::Result<Json<Snapshot>, ApiError> {
    authorize(&daemon, &headers)?;
    daemon
        .snapshot_pages(query.session.as_deref(), query.before, query.aside_before)
        .map(Json)
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

async fn command(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Json(command): Json<Command>,
) -> std::result::Result<Json<Receipt>, ApiError> {
    authorize(&daemon, &headers)?;
    daemon
        .execute(command)
        .map(Json)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorReadQuery {
    session: String,
    document: String,
    requirement: String,
    offset: usize,
}

async fn editor_read(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Query(query): Query<EditorReadQuery>,
) -> std::result::Result<Response, ApiError> {
    authorize(&daemon, &headers)?;
    if query.session.len() > 256 || query.document.len() > 256 || query.requirement.len() > 8192 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid editor metadata".into(),
        ));
    }
    let requirement = serde_json::from_str(&query.requirement)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid editor requirement".into()))?;
    let bytes = daemon
        .store
        .lock()
        .expect("store poisoned")
        .document_chunk(&query.session, &query.document, &requirement, query.offset)
        .map_err(|error| ApiError(StatusCode::CONFLICT, error.to_string()))?;
    let length = bytes.len();
    let mut response = (
        [
            ("content-type", "application/octet-stream"),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
        ],
        bytes,
    )
        .into_response();
    response.headers_mut().insert(
        "content-length",
        length.to_string().parse().expect("numeric header"),
    );
    Ok(response)
}

// Bound collection even before loading the engine's smaller legacy Save limit.
const EDITOR_UPLOAD_BYTES: usize = crate::documenttransfer::DOCUMENT_BYTES;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorUploadQuery {
    session: String,
    document: String,
    requirement: String,
    bytes: usize,
    sha256: String,
}

async fn editor_upload(
    State(daemon): State<Arc<Daemon>>,
    Query(query): Query<EditorUploadQuery>,
    request: axum::extract::Request,
) -> std::result::Result<Json<Receipt>, ApiError> {
    authorize(&daemon, request.headers())?;
    if query.bytes > EDITOR_UPLOAD_BYTES {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Document exceeds the editing limit".into(),
        ));
    }
    if query.session.len() > 256 || query.document.len() > 256 || query.requirement.len() > 8192 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid editor upload metadata".into(),
        ));
    }
    if query.sha256.len() != 64
        || !query
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Invalid editor upload checksum".into(),
        ));
    }
    if !request
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("text/plain"))
        })
    {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Editor uploads require UTF-8 text/plain".into(),
        ));
    }
    let requirement: serde_json::Value = serde_json::from_str(&query.requirement)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid editor requirement".into()))?;
    // Refuse stale or unauthorized requests before reading their bodies.
    {
        let store = daemon.store.lock().expect("store poisoned");
        let session = store
            .session(&query.session)
            .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
        daemon
            .ensure_remote_control(&session)
            .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
        if !session.approvals.iter().any(|editor| {
            editor.id == query.document
                && editor.requirement["kind"] == "plank_editor"
                && editor.requirement == requirement
        }) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "Editor changed; reopen before saving".into(),
            ));
        }
        if query.bytes > crate::commands::editor_save_limit(session.controls.as_ref()) {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "This engine does not support larger document Saves".into(),
            ));
        }
    }
    let body = axum::body::to_bytes(request.into_body(), query.bytes)
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Editor upload exceeds its limit or could not be read".into(),
            )
        })?;
    if body.len() != query.bytes || crate::documenttransfer::digest(&body) != query.sha256 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Document upload is incomplete or corrupt".into(),
        ));
    }
    let text = String::from_utf8(body.to_vec()).map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "Editor uploads must contain valid UTF-8".into(),
        )
    })?;
    // execute checks the same complete requirement again after collection,
    // including any generation, dialog or controller change during the upload.
    daemon
        .execute(Command::EditDocument {
            session_id: query.session,
            approval_id: query.document,
            requirement,
            text: Some(text),
        })
        .map(Json)
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))
}

#[derive(Debug, Deserialize)]
struct ExportQuery {
    session: String,
    generation: u64,
    id: String,
}

async fn export_file(
    State(daemon): State<Arc<Daemon>>,
    headers: HeaderMap,
    Query(query): Query<ExportQuery>,
) -> std::result::Result<Response, ApiError> {
    authorize(&daemon, &headers)?;
    let (metadata, file) = {
        let store = daemon.store.lock().expect("store poisoned");
        let current = store
            .session(&query.session)
            .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Export not found".into()))?;
        if current.generation != query.generation {
            return Err(ApiError(StatusCode::NOT_FOUND, "Export not found".into()));
        }
        daemon
            .artifacts
            .lock()
            .expect("artifacts poisoned")
            .file(&query.session, query.generation, &query.id)
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Export not found".into()))?
    };
    let stream = futures_util::stream::try_unfold((file, 0_u64), |(file, offset)| async move {
        let reader = Arc::clone(&file);
        let bytes = tokio::task::spawn_blocking(move || crate::artifacts::read(&reader, offset))
            .await
            .map_err(std::io::Error::other)??;
        if bytes.is_empty() {
            Ok::<_, std::io::Error>(None)
        } else {
            let next = offset + bytes.len() as u64;
            Ok(Some((bytes, (file, next))))
        }
    });
    let mut response = axum::body::Body::from_stream(stream).into_response();
    // Percent encoding prevents filename/header injection and preserves Unicode.
    let mut name = String::new();
    for byte in metadata.name.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(name, "%{byte:02X}");
    }
    let headers = response.headers_mut();
    headers.insert(
        "content-type",
        "application/octet-stream".parse().expect("static header"),
    );
    headers.insert(
        "content-disposition",
        format!(
            "attachment; filename=\"session.{}\"; filename*=UTF-8''{name}",
            metadata.format
        )
        .parse()
        .map_err(|_| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Invalid export filename".into(),
            )
        })?,
    );
    headers.insert(
        "content-length",
        metadata.bytes.to_string().parse().expect("numeric header"),
    );
    headers.insert("cache-control", "no-store".parse().expect("static header"));
    headers.insert(
        "x-content-type-options",
        "nosniff".parse().expect("static header"),
    );
    Ok(response)
}

/// Builds the authenticated daemon API.
pub fn router(daemon: Arc<Daemon>) -> Router {
    Router::new()
        .route("/v1/snapshot", get(snapshot))
        .route("/v1/command", post(command))
        .route("/v1/editor", get(editor_read).post(editor_upload))
        .route("/v1/export", get(export_file))
        .layer(axum::extract::DefaultBodyLimit::max(128 * 1024))
        .with_state(daemon)
}

/// Adds same-origin browser assets without exposing the daemon's private data.
///
/// The directory must contain trusted WASM packaging output. API authentication
/// still applies; only static HTML, JavaScript, and WASM assets are public.
pub fn router_with_web(daemon: Arc<Daemon>, directory: PathBuf) -> Router {
    use axum::{Extension, middleware};
    router(daemon)
        .fallback(crate::web_host::asset)
        .layer(Extension(directory))
        .layer(middleware::from_fn(crate::web_host::isolate))
}

/// Selects the default data directory without requiring a particular shell.
pub fn default_data_dir() -> PathBuf {
    std::env::var_os("NPLANK_DATA_DIR").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".nplank"),
        PathBuf::from,
    )
}

/// Reads a private bearer token, creating it on first launch.
///
/// # Errors
/// Returns filesystem errors or invalid token data.
pub fn token_in(directory: &Path) -> Result<String> {
    std::fs::create_dir_all(directory)?;
    let path = directory.join("token");
    let token = match std::fs::read_to_string(&path) {
        Ok(token) => token,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Write;
            let token = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            file.write_all(token.as_bytes())?;
            file.sync_all()?;
            token
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(!token.trim().is_empty(), "Token file is empty");
    Ok(token.trim().into())
}

/// Runs one loopback daemon, refusing concurrent owners of its database.
///
/// # Errors
/// Returns lock, database, address, or server errors.
pub async fn serve(directory: &Path, address: SocketAddr) -> Result<()> {
    serve_with_web(directory, address, None).await
}

/// Runs the daemon with an optional same-origin browser client.
///
/// # Errors
/// Returns invalid asset-directory, lock, database, address, or server errors.
pub async fn serve_with_web(
    directory: &Path,
    address: SocketAddr,
    web_directory: Option<&Path>,
) -> Result<()> {
    ensure!(
        address.ip().is_loopback(),
        "Bind to loopback and use an SSH tunnel for remote access"
    );
    std::fs::create_dir_all(directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("Another daemon owns this data directory")?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    let daemon = Daemon::open(&directory.join("state.sqlite"), token_in(directory)?)?;
    eprintln!("nplankd listening on {}", listener.local_addr()?);
    let app = if let Some(web_directory) = web_directory {
        let web_directory = std::fs::canonicalize(web_directory)
            .context("Browser assets not found; run just wasm-build first")?;
        ensure!(
            web_directory.join("index.html").is_file(),
            "Browser index.html is missing"
        );
        router_with_web(daemon, web_directory)
    } else {
        router(daemon)
    };
    axum::serve(listener, app).await?;
    Ok(())
}
