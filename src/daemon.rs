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
        let mut snapshot = self
            .store
            .lock()
            .expect("store poisoned")
            .snapshot(id, before)?;
        snapshot.providers = crate::agents::providers();
        snapshot.protocol_version = crate::model::PROTOCOL_VERSION;
        let telemetry = self.telemetry.lock().expect("telemetry poisoned");
        for session in &snapshot.sessions {
            if !session.recovery_required
                && !matches!(session.state, RunState::Failed | RunState::Interrupted)
                && let Some((generation, counters)) = telemetry.get(&session.id)
                && *generation == session.generation
            {
                snapshot
                    .telemetry
                    .insert(session.id.clone(), counters.clone());
            }
        }
        drop(telemetry);
        // Verified files outlive live counters and socket attachments. Reconstruct
        // their small descriptors without resurrecting live engine status.
        let artifacts = self.artifacts.lock().expect("artifacts poisoned");
        for session in &snapshot.sessions {
            for export in artifacts.metadata(&session.id, session.generation) {
                let counters = snapshot.telemetry.entry(session.id.clone()).or_default();
                if export.purpose == crate::model::ExportPurpose::Insights {
                    counters.insights_export = Some(export);
                } else {
                    counters.export = Some(export);
                }
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

    /// Applies a command, persisting prompts before execution begins.
    ///
    /// # Errors
    /// Returns validation, storage, and dispatch errors.
    pub fn execute(self: &Arc<Self>, command: Command) -> Result<Receipt> {
        let _command = self.commands.lock().expect("commands poisoned");
        let id = match command {
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
                ensure!(
                    !session.state.busy() || (request && waiting),
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
                    text.as_ref().is_none_or(|text| text.len() <= 256 * 1024),
                    "Document exceeds the editing limit"
                );
                let store = self.store.lock().expect("store poisoned");
                let session = store.session(&session_id)?;
                self.ensure_remote_control(&session)?;
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

#[derive(Debug, Default, Deserialize)]
struct SnapshotQuery {
    session: Option<String>,
    #[serde(default)]
    before: i64,
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
        .snapshot(query.session.as_deref(), query.before)
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
