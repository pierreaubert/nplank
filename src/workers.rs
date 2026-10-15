//! Isolated headless Plank processes and remote-control protocol adapters.

// Rust guideline compliant 2026-02-21
use std::{process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    daemon::Daemon,
    model::{Backend, RunState, Session},
};

#[derive(Debug)]
pub(crate) enum Input {
    Observe,
    RemoteControl(bool),
    Prompt(String),
    Aside(String),
    LiveControl(crate::livecontrol::LiveControl),
    FrameControl(crate::frame::Request),
    StopJobOutput(String),
    Stop,
    Approval {
        approval: crate::model::PendingApproval,
        choice: String,
    },
}

pub(crate) async fn run(
    daemon: Arc<Daemon>,
    session: &Session,
    directory: &str,
    mut input: mpsc::Receiver<Input>,
) -> Result<()> {
    let _exports = ExportLease {
        daemon: Arc::downgrade(&daemon),
        session: session.id.clone(),
        generation: session.generation,
    };
    match &session.backend {
        Backend::Demo => {
            while let Some(Input::Prompt(prompt)) = input.recv().await {
                state(&daemon, session, RunState::Running)?;
                let answer = format!(
                    "Demo session: {}\n\nYou wrote: {prompt}\n\nThis Rust worker runs in the daemon. Closing the window leaves it running. Select Local Plank to use a real engine.",
                    session.name
                );
                for word in answer.split_inclusive(' ') {
                    append(&daemon, session, "assistant", word)?;
                    tokio::time::sleep(Duration::from_millis(65)).await;
                }
                state(&daemon, session, RunState::Idle)?;
            }
            Ok(())
        }
        Backend::Local { executable, args } => {
            local(daemon, session, directory, executable, args, input).await
        }
        Backend::Agent { .. } => crate::agents::run(daemon, session, directory, input).await,
        Backend::Remote { url, token_env } => {
            let initial = match input.recv().await {
                Some(Input::Prompt(prompt)) => Some(prompt),
                Some(Input::Observe) => None,
                _ => return Ok(()),
            };
            remote(
                Arc::clone(&daemon),
                session,
                url,
                token_env,
                initial.as_deref(),
                &mut input,
            )
            .await?;
            Ok(())
        }
    }
}

/// Retires live frames and incomplete exports when their worker ends.
struct ExportLease {
    daemon: std::sync::Weak<Daemon>,
    session: String,
    generation: u64,
}

impl Drop for ExportLease {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.upgrade() {
            if let Ok(mut artifacts) = daemon.artifacts.lock() {
                artifacts.discard_pending(&self.session, self.generation);
            }
            if let Ok(mut telemetry) = daemon.telemetry.lock()
                && let Some((owner, counters)) = telemetry.get_mut(&self.session)
                && *owner == self.generation
            {
                counters.frame = None;
            }
        }
    }
}

fn append(daemon: &Daemon, session: &Session, role: &str, text: &str) -> Result<()> {
    let store = daemon.store.lock().expect("store poisoned");
    if store.session(&session.id)?.generation == session.generation {
        store.append(&session.id, role, text)?;
    }
    Ok(())
}

fn state(daemon: &Daemon, session: &Session, state: RunState) -> Result<()> {
    let store = daemon.store.lock().expect("store poisoned");
    if store.session(&session.id)?.generation == session.generation {
        store.set_state(&session.id, state, false)?;
    }
    Ok(())
}

async fn local(
    daemon: Arc<Daemon>,
    session: &Session,
    directory: &str,
    executable: &str,
    args: &[String],
    mut input: mpsc::Receiver<Input>,
) -> Result<()> {
    let output_control = tempfile::Builder::new()
        .prefix("nplank-output-control-")
        .tempdir()?;
    let output_mailbox = output_control.path().join("stop.json");
    let focus_mailbox = output_control.path().join("focus.json");
    let mut child = tokio::process::Command::new(executable)
        .args(args)
        .args(["--ui", "console"])
        .env("NO_COLOR", "1")
        .env("PLANK_GUI_PROTOCOL", "1")
        .env("PLANK_GUI_NOTIFICATIONS", "1")
        .env("PLANK_GUI_EXPORT_STREAM", "1")
        .env("PLANK_GUI_EDITOR_STREAM", "1")
        .env("PLANK_GUI_JOB_CONTROL", &output_mailbox)
        .env("PLANK_GUI_FOCUS_CONTROL", &focus_mailbox)
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Cannot start Plank; check the executable and engine arguments")?;
    let _process = crate::metadata::ProcessLease::new(&daemon, session, child.id());
    let mut stdin = child.stdin.take().context("Missing engine stdin")?;
    let mut writes = ConsoleWrites::default();
    let mut stdout = child.stdout.take().context("Missing engine stdout")?;
    let mut stderr = BufReader::new(child.stderr.take().context("Missing engine stderr")?);
    let mut bytes = [0_u8; 4096];
    let mut stderr_line = Vec::new();
    let mut pending_bytes = Vec::new();
    let mut ready = false;
    let mut booting = true;
    let mut running = false;
    let mut deferred_prompt = None;
    let mut question = false;
    let mut question_id = String::new();
    let mut editor_opening = EditorOpeningInbox::default();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut quit_requested = false;
    let mut typed_prompts = false;
    let mut document_save: Option<(
        crate::model::PendingApproval,
        crate::documenttransfer::Sender<'static>,
    )> = None;
    let mut exit_announced = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    // Bound pipe draining after process exit: descendants can retain a pipe.
    let exit_drain = tokio::time::sleep(Duration::from_secs(2));
    tokio::pin!(exit_drain);
    // Startup must reach the documented readiness marker rather than hanging forever.
    let startup = tokio::time::sleep(Duration::from_secs(180));
    tokio::pin!(startup);
    let mut focus_tick = tokio::time::interval(Duration::from_secs(1));
    focus_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if let Some(status) = exit_status
            && !stdout_open
            && !stderr_open
        {
            ensure!(
                quit_requested && exit_announced && status.success(),
                "Plank exited ({status}) and its live context was lost"
            );
            daemon.local_engine_exited(session)?;
            return Ok(());
        }
        tokio::select! {
            _ = std::future::ready(()), if document_save.is_some() && exit_status.is_none() && writes.is_empty() => {
                let (approval, frames) = document_save.as_mut().context("Missing document transfer")?;
                let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                if current.generation != session.generation || !question || question_id != approval.id
                    || !current.approvals.iter().any(|pending| pending.id == approval.id && pending.requirement == approval.requirement)
                {
                    document_save = None;
                    continue;
                }
                if let Some(frame) = frames.next() {
                    let reply = serde_json::json!({"type":"dialog_reply","id":approval.id,"text":frame});
                    writes.json(&reply)?;
                } else {
                    document_save = None;
                }
                // A sent End is not an editor acknowledgment. Retain the form
                // until the engine closes its matching request.
                continue;
            }
            written = stdin.write(writes.front()), if !writes.is_empty() && exit_status.is_none() => {
                writes.advance(written?)?;
            }
            _ = &mut exit_drain, if exit_status.is_some() => bail!("Plank exited before its output pipes closed"),
            _ = focus_tick.tick() => {
                // Notification preference failures must never terminate an agent.
                // A stale packet expires independently in the child process.
                let _ = publish_focus(&daemon, session, &focus_mailbox).await;
            }
            _ = &mut startup, if booting && !question => bail!("Plank did not announce console readiness within three minutes"),
            message = input.recv(), if exit_status.is_none() && (ready || question || running) => {
                match message {
                    Some(Input::LiveControl(request)) if !booting => {
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        if current.generation != session.generation || !current.can_receive_live_control(request) { continue; }
                        let frame = serde_json::json!({"type":"live_control","request":request});
                        writes.json(&frame)?;
                    }
                    Some(Input::Aside(question)) if !booting => {
                        let frame = serde_json::json!({"type":"btw","text":question});
                        writes.json(&frame)?;
                    }
                    Some(Input::Prompt(prompt)) if ready => {
                        quit_requested = matches!(prompt.trim(), "/quit" | "/exit");
                        writes.prompt(&prompt, typed_prompts)?;
                        running = true;
                        ready = false;
                        state(&daemon, session, RunState::Running)?;
                    }
                    Some(Input::Prompt(prompt)) if question && !running => {
                        ensure!(deferred_prompt.is_none(), "Multiple startup prompts were queued");
                        deferred_prompt = Some(prompt);
                    }
                    Some(Input::Approval { approval, choice }) if question && approval.id == question_id => {
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        if current.generation != session.generation || !current.approvals.iter().any(|pending| pending.id == approval.id && pending.requirement == approval.requirement) {
                            continue;
                        }
                        document_save = None;
                        let streaming = current.controls.as_ref().is_some_and(|controls| controls.features.iter()
                            .any(|feature| feature.name == "editor_stream_save" && feature.available));
                        if let Some(frames) = document_save_frames(&approval, &choice, streaming)? {
                            document_save = Some((approval, frames));
                            continue;
                        }
                        let reply = if approval.requirement["kind"] == "plank_editor" {
                            let text: Option<String> = serde_json::from_str(&choice)?;
                            serde_json::json!({"type":"dialog_reply", "id": approval.id, "text": text})
                        } else {
                            ensure!(approval.requirement["kind"] == "plank_question", "Wrong approval protocol");
                            serde_json::json!({"type":"dialog_reply", "id": approval.id, "answer": choice})
                        };
                        writes.json(&reply)?;
                        daemon.store.lock().expect("store poisoned")
                            .resolve_approval(&session.id, session.generation, &approval.id)?;
                        question = false;
                        startup.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(180));
                    }
                    Some(Input::StopJobOutput(observer)) => {
                        if job_observer_current(&daemon, session, &observer)? {
                            let temporary = output_control.path().join("stop.tmp");
                            tokio::fs::write(&temporary, serde_json::to_vec(&serde_json::json!({"observer":observer}))?).await?;
                            tokio::fs::rename(&temporary, &output_mailbox).await?;
                        }
                    }
                    Some(Input::FrameControl(request)) => {
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        let valid = current.generation == session.generation
                            && request.is_valid()
                            && daemon.telemetry.lock().expect("telemetry poisoned")
                                .get(&session.id).filter(|(owner, _)| *owner == session.generation)
                                .and_then(|(_, telemetry)| telemetry.frame.as_ref())
                                .is_some_and(|frame| frame.id == request.id);
                        if !valid { continue; }
                        let mut frame = serde_json::to_value(request)?;
                        frame["type"] = "frame_input".into();
                        writes.json(&frame)?;
                    }
                    Some(Input::Approval { .. }) => continue,
                    _ => return Ok(()),
                }
            }
            read = stdout.read(&mut bytes), if stdout_open => {
                let count = read?;
                stdout_open = count != 0;
                if count != 0 {
                    pending_bytes.extend_from_slice(&bytes[..count]);
                    let text = decode_utf8(&mut pending_bytes);
                    if !text.is_empty() { append(&daemon, session, "assistant", &text)?; }
                }
            }
            // read_until retains partially consumed bytes when another select branch wins.
            read = stderr.read_until(b'\n', &mut stderr_line), if stderr_open => {
                stderr_open = read? != 0;
                if !stderr_open && daemon.telemetry.lock().expect("telemetry poisoned")
                    .get(&session.id).filter(|(owner, _)| *owner == session.generation)
                    .is_some_and(|(_, counters)| counters.frame.is_some()) {
                    bail!("Plank frame control channel closed before frame completion");
                }
                ensure!(stderr_line.len() <= 2 * 1024 * 1024, "Engine stderr line is too large");
                let line = std::str::from_utf8(&stderr_line)?;
                if line.trim() == "+DWARFSTAR_WAITING" {
                    editor_opening = EditorOpeningInbox::default();
                    booting = false;
                    if question {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                        question = false;
                    }
                    ready = true;
                    if running {
                        state(&daemon, session, RunState::Idle)?;
                        running = false;
                    }
                    if let Some(prompt) = deferred_prompt.take() {
                        quit_requested = matches!(prompt.trim(), "/quit" | "/exit");
                        writes.prompt(&prompt, typed_prompts)?;
                        running = true;
                        ready = false;
                        state(&daemon, session, RunState::Running)?;
                    }
                } else if line.trim() == "+PLANK_EXITING" {
                    ensure!(quit_requested, "Plank announced an unrequested engine exit");
                    exit_announced = true;
                } else if line.trim() == "+PLANK_BUSY" {
                    ensure!(!booting, "Plank announced remote work before console readiness");
                    ready = false;
                    running = true;
                    state(&daemon, session, RunState::Running)?;
                } else if line.trim() == "+PLANK_RESTARTING" {
                    editor_opening = EditorOpeningInbox::default();
                    document_save = None;
                    typed_prompts = false;
                    daemon.telemetry.lock().expect("telemetry poisoned").remove(&session.id);
                    booting = true;
                    startup.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(180));
                    question = false;
                    daemon.store.lock().expect("store poisoned").engine_restarting(&session.id, session.generation)?;
                } else if let Some(launch) = line.trim().strip_prefix("+PLANK_LAUNCH ") {
                    ensure!(launch.len() <= 2 * 1024 * 1024, "Engine launch is too large");
                    #[derive(serde::Deserialize)]
                    struct Launch { args: Vec<String> }
                    let launch: Launch = serde_json::from_str(launch)?;
                    daemon.store.lock().expect("store poisoned").engine_launch(&session.id, session.generation, launch.args)?;
                } else if let Some(notification) = line.trim().strip_prefix("+PLANK_NOTIFY ") {
                    // Notifications are advisory: malformed payloads cannot kill a turn.
                    if notification.len() <= 32 * 1024 && let Ok(value) = serde_json::from_str(notification) {
                        ingest_local_notification(&daemon, session, &value)?;
                    }
                } else if let Some(telemetry) = line.trim().strip_prefix("+PLANK_TELEMETRY ") {
                    ensure!(telemetry.len() <= 2 * 1024 * 1024, "Engine telemetry is too large");
                    let value: serde_json::Value = serde_json::from_str(telemetry)?;
                    if value["type"] == "reset" {
                        editor_opening = EditorOpeningInbox::default();
                        document_save = None;
                    }
                    ingest_telemetry(&daemon, session, &value)?;
                } else if let Some(progress) = line.trim().strip_prefix("+PLANK_PROGRESS ") {
                    ensure!(progress.len() <= 4096, "Engine progress is too large");
                    daemon.store.lock().expect("store poisoned").engine_progress(&session.id, session.generation, serde_json::from_str(progress)?)?;
                } else if let Some(done) = line.trim().strip_prefix("+PLANK_QUESTION_DONE ").or_else(|| line.trim().strip_prefix("+PLANK_EDITOR_DONE ")) {
                    ensure!(done.len() <= 1024, "Question completion is too large");
                    let done: serde_json::Value = serde_json::from_str(done)?;
                    if let Some(id) = done["id"].as_str() { editor_opening.cancel(id); }
                    if question && done["id"].as_str() == Some(question_id.as_str()) {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                        question = false;
                        startup.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(180));
                    }
                } else if let Some(catalog) = line.trim().strip_prefix("+PLANK_CONTROLS ") {
                    ensure!(catalog.len() <= 1024 * 1024, "Engine catalog is too large");
                    let controls: crate::commands::EngineControls = serde_json::from_str(catalog)?;
                    typed_prompts = controls.features.iter().any(|feature| feature.name == "console_prompts" && feature.available);
                    daemon.store.lock().expect("store poisoned")
                        .controls(&session.id, session.generation, controls)?;
                } else if let Some(request) = line.trim().strip_prefix("+PLANK_EDITOR_STREAM ") {
                    if let Some(editor) = editor_opening.feed(request, session.generation)? {
                        if question {
                            daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                        }
                        question_id.clone_from(&editor.id);
                        daemon.store.lock().expect("store poisoned").approval(&session.id, session.generation, editor)?;
                        question = true;
                    }
                } else if let Some(request) = line.trim().strip_prefix("+PLANK_EDITOR ") {
                    editor_opening = EditorOpeningInbox::default();
                    let editor = plank_editor(request, session.generation)?;
                    if question {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                    }
                    question_id.clone_from(&editor.id);
                    daemon.store.lock().expect("store poisoned").approval(&session.id, session.generation, editor)?;
                    question = true;
                } else if let Some(request) = line.trim().strip_prefix("+PLANK_QUESTION ") {
                    editor_opening = EditorOpeningInbox::default();
                    if question {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                    }
                    let approval = plank_question(request, session.generation)?;
                    question_id.clone_from(&approval.id);
                    daemon.store.lock().expect("store poisoned")
                        .approval(&session.id, session.generation, approval)?;
                    question = true;
                } else if !line.trim().is_empty() {
                    append(&daemon, session, "diagnostic", line)?;
                }
                stderr_line.clear();
            }
            exit = child.wait(), if exit_status.is_none() => {
                exit_status = Some(exit?);
                exit_drain.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(2));
            }
        }
    }
}

/// Preserves frame boundaries across cancellable, partial console writes.
#[derive(Default)]
struct ConsoleWrites {
    frames: std::collections::VecDeque<Vec<u8>>,
    offset: usize,
    bytes: usize,
}

impl ConsoleWrites {
    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn front(&self) -> &[u8] {
        self.frames
            .front()
            .map_or(&[], |frame| &frame[self.offset..])
    }

    fn json(&mut self, value: &serde_json::Value) -> Result<()> {
        self.push(serde_json::to_vec(value)?)
    }

    fn prompt(&mut self, text: &str, framed: bool) -> Result<()> {
        if framed {
            self.json(&serde_json::json!({"type":"prompt","text":text}))
        } else {
            self.push(text.as_bytes().to_vec())
        }
    }

    fn push(&mut self, mut frame: Vec<u8>) -> Result<()> {
        // Match the engine's 2 MiB frame and 4 MiB pending-input limits.
        // Count complete retained frames, including an already written prefix.
        frame.push(b'\n');
        ensure!(frame.len() <= 2 * 1024 * 1024, "Console frame is too large");
        ensure!(
            self.frames.len() < 64 && self.bytes.saturating_add(frame.len()) <= 4 * 1024 * 1024,
            "Console write queue is full; inspect the engine before retrying"
        );
        self.bytes += frame.len();
        self.frames.push_back(frame);
        Ok(())
    }

    fn advance(&mut self, count: usize) -> Result<()> {
        ensure!(
            count > 0 && count <= self.front().len(),
            "Engine stdin closed during submission"
        );
        self.offset += count;
        if self
            .frames
            .front()
            .is_some_and(|frame| self.offset == frame.len())
        {
            if let Some(frame) = self.frames.pop_front() {
                self.bytes -= frame.len();
            }
            self.offset = 0;
        }
        Ok(())
    }
}

fn plank_editor(text: &str, generation: u64) -> Result<crate::model::PendingApproval> {
    ensure!(text.len() <= 2 * 1024 * 1024, "Editor request is too large");
    let request: serde_json::Value = serde_json::from_str(text)?;
    let text = request["text"].as_str().context("Missing document text")?;
    ensure!(
        text.len() <= 256 * 1024,
        "Inline document exceeds the editing limit; use chunked opening"
    );
    let id = request["id"].as_str().context("Missing editor ID")?;
    let title = request["title"].as_str().unwrap_or("Document");
    plank_editor_value(id.into(), title.into(), text.into(), generation)
}

fn plank_editor_value(
    id: String,
    title: String,
    text: String,
    generation: u64,
) -> Result<crate::model::PendingApproval> {
    ensure!(
        text.len() <= crate::documenttransfer::DOCUMENT_BYTES,
        "Document exceeds the editing limit"
    );
    ensure!(!id.is_empty() && id.len() <= 256, "Invalid editor ID");
    ensure!(title.len() <= 4096, "Editor title is too large");
    Ok(crate::model::PendingApproval {
        document: None,
        id,
        requirement: serde_json::json!({"kind":"plank_editor", "generation":generation, "revision":uuid::Uuid::new_v4().to_string()}),
        tool: title,
        arguments: text,
        choices: Vec::new(),
    })
}

// Accommodate worst-case JSON escaping for a 32 KiB chunk, 4 KiB title,
// 256-byte editor ID and transfer metadata without approaching 2 MiB frames.
const EDITOR_STREAM_FRAME_BYTES: usize = crate::documenttransfer::CHUNK_BYTES * 6 + 32 * 1024;

#[derive(Default)]
struct EditorOpeningInbox(Option<EditorOpening>);

struct EditorOpening {
    id: String,
    title: String,
    receiver: crate::documenttransfer::Receiver,
}

impl EditorOpeningInbox {
    fn cancel(&mut self, id: &str) {
        if self.0.as_ref().is_some_and(|pending| pending.id == id) {
            self.0 = None;
        }
    }

    fn feed(
        &mut self,
        text: &str,
        generation: u64,
    ) -> Result<Option<crate::model::PendingApproval>> {
        ensure!(
            text.len() <= EDITOR_STREAM_FRAME_BYTES,
            "Editor transfer frame is too large"
        );
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Frame {
            id: String,
            title: String,
            frame: crate::documenttransfer::Frame,
        }
        let frame: Frame = serde_json::from_str(text)?;
        if let crate::documenttransfer::Frame::Begin { total, .. } = &frame.frame {
            ensure!(
                !frame.id.is_empty() && frame.id.len() <= 256 && frame.title.len() <= 4096,
                "Invalid editor transfer metadata"
            );
            ensure!(
                *total <= crate::documenttransfer::DOCUMENT_BYTES,
                "Document exceeds the editing limit"
            );
            self.0 = Some(EditorOpening {
                id: frame.id.clone(),
                title: frame.title.clone(),
                receiver: Default::default(),
            });
        }
        let Some(pending) = self.0.as_mut().filter(|pending| pending.id == frame.id) else {
            return Ok(None);
        };
        ensure!(
            pending.title == frame.title,
            "Editor transfer metadata changed"
        );
        if let Some(text) = pending
            .receiver
            .accept(&frame.frame)
            .map_err(anyhow::Error::msg)?
        {
            let pending = self.0.take().context("Missing editor transfer")?;
            return plank_editor_value(pending.id, pending.title, text, generation).map(Some);
        }
        Ok(None)
    }
}

fn plank_question(text: &str, generation: u64) -> Result<crate::model::PendingApproval> {
    use crate::model::{ApprovalChoice, PendingApproval};
    ensure!(text.len() <= 256 * 1024, "Plank question is too large");
    let request: serde_json::Value = serde_json::from_str(text)?;
    let id = request["id"]
        .as_str()
        .context("Missing Plank question ID")?;
    ensure!(
        !id.is_empty() && id.len() <= 256,
        "Invalid Plank question ID"
    );
    let options = request["options"]
        .as_array()
        .context("Missing question choices")?;
    ensure!(
        !options.is_empty() && options.len() <= 32,
        "Invalid question choices"
    );
    let mut choices = options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let label = option["label"].as_str().context("Missing choice label")?;
            let description = option["description"].as_str().unwrap_or("");
            Ok(ApprovalChoice {
                id: (index + 1).to_string(),
                label: if description.is_empty() {
                    label.to_owned()
                } else {
                    format!("{label} — {description}")
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    choices.push(ApprovalChoice {
        id: "0".into(),
        label: "Decline".into(),
    });
    Ok(PendingApproval {
        document: None,
        id: id.to_owned(),
        requirement: serde_json::json!({"kind": "plank_question", "generation": generation, "revision": uuid::Uuid::new_v4().to_string(), "request": request}),
        tool: request["header"].as_str().unwrap_or("Question").to_owned(),
        arguments: request["question"]
            .as_str()
            .context("Missing question text")?
            .to_owned(),
        choices,
    })
}

fn decode_utf8(pending: &mut Vec<u8>) -> String {
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(text) => {
                output.push_str(text);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                if let Ok(text) = std::str::from_utf8(&pending[..valid]) {
                    output.push_str(text);
                }
                pending.drain(..valid);
                if let Some(invalid) = error.error_len() {
                    output.push('\u{fffd}');
                    pending.drain(..invalid);
                } else {
                    break;
                }
            }
        }
    }
    output
}

async fn remote(
    daemon: Arc<Daemon>,
    session: &Session,
    url: &str,
    token_env: &str,
    prompt: Option<&str>,
    input: &mut mpsc::Receiver<Input>,
) -> Result<()> {
    let token = std::env::var(token_env)
        .context("Remote token environment variable is not set in the daemon")?;
    remote_with_connector(daemon, session, &token, prompt, input, || async {
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(url),
        )
        .await??;
        Ok(socket)
    })
    .await
}

#[derive(Debug, PartialEq, Eq)]
enum RemoteExit {
    Closed,
    Restart(String),
    AwaitReplacement,
}

async fn remote_with_connector<S, C, F>(
    daemon: Arc<Daemon>,
    session: &Session,
    token: &str,
    prompt: Option<&str>,
    input: &mut mpsc::Receiver<Input>,
    mut connect: C,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    C: FnMut() -> F,
    F: std::future::Future<Output = Result<tokio_tungstenite::WebSocketStream<S>>>,
{
    let mut initial = prompt;
    let mut replaced = None;
    let mut restart_deadline = None;
    loop {
        if !remote_generation_current(&daemon, session)? {
            return Ok(());
        }
        let socket = if replaced.is_some() {
            let Some(socket) = reconnect_remote(
                &daemon,
                session,
                input,
                &mut connect,
                restart_deadline.expect("restart has a reconnect deadline"),
            )
            .await?
            else {
                state(&daemon, session, RunState::Idle)?;
                return Ok(());
            };
            socket
        } else {
            connect().await?
        };
        if !remote_generation_current(&daemon, session)? {
            return Ok(());
        }
        match remote_connection(
            Arc::clone(&daemon),
            session,
            socket,
            token,
            initial.take(),
            input,
            replaced.as_deref(),
        )
        .await?
        {
            RemoteExit::Closed => return Ok(()),
            RemoteExit::Restart(instance) => {
                replaced = Some(instance);
                // Binding precedes model loading. Bound old-engine cleanup
                // and process replacement independently of model warmup.
                restart_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(90));
            }
            RemoteExit::AwaitReplacement => {}
        }
    }
}

async fn reconnect_remote<S, C, F>(
    daemon: &Daemon,
    session: &Session,
    input: &mut mpsc::Receiver<Input>,
    connect: &mut C,
    deadline: tokio::time::Instant,
) -> Result<Option<tokio_tungstenite::WebSocketStream<S>>>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
    C: FnMut() -> F,
    F: std::future::Future<Output = Result<tokio_tungstenite::WebSocketStream<S>>>,
{
    loop {
        if !remote_generation_current(daemon, session)? {
            return Ok(None);
        }
        // Bound retry traffic, including handshakes reaching the old instance.
        // Stop and the overall deadline also interrupt this short delay.
        tokio::select! {
            biased;
            command = input.recv() => {
                ensure!(matches!(command, None | Some(Input::Stop)),
                    "Remote is restarting; queued input was not sent. Inspect the host before retrying");
                return Ok(None);
            }
            _ = tokio::time::sleep_until(deadline) => {
                bail!("Remote did not reopen after its restart; inspect the host before retrying");
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {},
        }
        tokio::select! {
            biased;
            command = input.recv() => {
                ensure!(matches!(command, None | Some(Input::Stop)),
                    "Remote is restarting; queued input was not sent. Inspect the host before retrying");
                return Ok(None);
            }
            attempt = tokio::time::timeout_at(deadline, connect()) => {
                match attempt {
                    Ok(Ok(socket)) => return Ok(Some(socket)),
                    Ok(Err(_)) => {},
                    Err(_) => bail!("Remote did not reopen after its restart; inspect the host before retrying"),
                }
            }
        }
    }
}

#[cfg(test)]
async fn remote_socket<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    daemon: Arc<Daemon>,
    session: &Session,
    socket: tokio_tungstenite::WebSocketStream<S>,
    token: &str,
    prompt: Option<&str>,
    input: &mut mpsc::Receiver<Input>,
) -> Result<()> {
    remote_connection(daemon, session, socket, token, prompt, input, None)
        .await
        .map(|_| ())
}

async fn remote_connection<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    daemon: Arc<Daemon>,
    session: &Session,
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    token: &str,
    prompt: Option<&str>,
    input: &mut mpsc::Receiver<Input>,
    replaced: Option<&str>,
) -> Result<RemoteExit> {
    socket
        .send(Message::Text(
            serde_json::json!({"v":1,"type":if prompt.is_some() {"auth"} else {"observe_auth"},"token":token,"export_stream":true,"frame_stream":true,"buddy_stream":true,"editor_stream":true})
                .to_string()
                .into(),
        ))
        .await?;
    let hello = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await?
        .context("Remote closed before authentication")??;
    let hello: serde_json::Value = serde_json::from_str(hello.to_text()?)?;
    ensure!(
        hello["type"] == "hello" && hello["protocol_version"] == 1,
        "Unsupported remote-control protocol"
    );
    let mut access = crate::model::RemoteAccess {
        client_id: hello["session_id"].as_u64(),
        controller: hello["controller"]
            .as_bool()
            .context("Missing remote role")?,
        requesting: false,
        waiting_input: prompt.is_some(),
        reason: None,
    };
    ensure!(
        prompt.is_some() || !access.controller,
        "Host granted control to observer authentication; rebuild the host"
    );
    ensure!(
        hello["instance_id"].as_str() != Some(token),
        "Remote server instance identity must be independent of authentication credentials"
    );
    if let Some(prior) = replaced {
        let instance = hello["instance_id"]
            .as_str()
            .context("Remote replacement has no server instance identity")?;
        if instance == prior {
            return Ok(RemoteExit::AwaitReplacement);
        }
    }
    bind_remote_instance(&daemon, session, &hello)?;
    let capabilities = RemoteInputCaps::from_hello(&hello);
    let mut editor_opening = EditorOpeningInbox::default();
    // Authenticate and consume the initial replay before submitting new input.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await?
            .context("Remote closed before snapshot")??;
        if !frame.is_text() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(frame.to_text()?)?;
        ingest_remote_opening(&daemon, session, &value, &mut editor_opening, false)?;
        if value["type"] == "snapshot" {
            break;
        }
    }
    if daemon
        .store
        .lock()
        .expect("store poisoned")
        .session(&session.id)?
        .generation
        != session.generation
    {
        return Ok(RemoteExit::Closed);
    }
    let mut pending = prompt.map(str::to_owned);
    if let Some(prompt) = &pending {
        remote_payload(prompt, capabilities)?;
    }
    let mut active = None;
    let mut active_request = None;
    if access.controller {
        if let Some(prompt) = pending.take() {
            let (payload, request_id) = remote_payload(&prompt, capabilities)?;
            socket
                .send(Message::Text(payload.to_string().into()))
                .await?;
            active_request = request_id;
            active = Some(prompt);
        }
    } else if pending.is_some() {
        socket
            .send(Message::Text(
                serde_json::json!({"v":1,"type":"request_control"})
                    .to_string()
                    .into(),
            ))
            .await?;
        access.requesting = true;
    }
    access.waiting_input = pending.is_some();
    publish_access(&daemon, session, &access)?;
    state(
        &daemon,
        session,
        if active.is_some() {
            RunState::Running
        } else if pending.is_some() {
            RunState::Starting
        } else {
            RunState::Idle
        },
    )?;
    let mut restarting = None;
    let mut restart_close_deadline = None;
    let mut document_save: Option<(
        crate::model::PendingApproval,
        crate::documenttransfer::Sender<'static>,
    )> = None;
    loop {
        let frame = tokio::select! {
            _ = std::future::ready(()), if document_save.is_some() => {
                let (approval, frames) = document_save.as_mut().context("Missing document transfer")?;
                let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                if !access.controller || current.generation != session.generation
                    || !current.approvals.iter().any(|pending| pending.id == approval.id && pending.requirement == approval.requirement)
                {
                    document_save = None;
                    continue;
                }
                if let Some(frame) = frames.next() {
                    let payload = serde_json::json!({"v":1,"type":"edit","document_id":approval.id,"text":frame});
                    socket.send(Message::Text(payload.to_string().into())).await?;
                } else {
                    document_save = None;
                }
                // Return to the multiplexed loop after every bounded frame,
                // allowing Stop, role changes and host replies between chunks.
                continue;
            }
            _ = tokio::time::sleep_until(restart_close_deadline.unwrap_or_else(tokio::time::Instant::now)), if restarting.is_some() => {
                // Completion is proven and restart is explicit. A missing bye
                // must not strand the UI on an old connection indefinitely.
                return Ok(RemoteExit::Restart(restarting.expect("restart notification has an instance")));
            }
            frame = socket.next() => match frame {
                Some(Ok(frame)) => frame,
                None | Some(Err(_)) if restarting.is_some() => {
                    return Ok(RemoteExit::Restart(restarting.unwrap()));
                }
                Some(Err(error)) => return Err(error.into()),
                None => bail!("Remote disconnected; inspect its session before retrying"),
            },
            command = input.recv() => {
                ensure!(restarting.is_none() || matches!(&command, None | Some(Input::Stop)),
                    "Remote is restarting; queued input was not sent. Inspect the host before retrying");
                match command {
                    Some(Input::Prompt(prompt)) => {
                        ensure!(active.is_none() && pending.is_none(), "Remote already has an active or queued input");
                        if !remote_generation_current(&daemon, session)? {
                            socket.close(None).await?;
                            return Ok(RemoteExit::Closed);
                        }
                        remote_payload(&prompt, capabilities)?;
                        if access.controller {
                            let (payload, request_id) = remote_payload(&prompt, capabilities)?;
                            socket.send(Message::Text(payload.to_string().into())).await?;
                            active_request = request_id;
                            active = Some(prompt);
                            state(&daemon, session, RunState::Running)?;
                        } else {
                            pending = Some(prompt);
                            access.waiting_input = true;
                            access.requesting = true;
                            access.reason = None;
                            socket.send(Message::Text(serde_json::json!({"v":1,"type":"request_control"}).to_string().into())).await?;
                            publish_access(&daemon, session, &access)?;
                            state(&daemon, session, RunState::Starting)?;
                        }
                        continue;
                    }
                    Some(Input::Aside(question)) => {
                        if !remote_generation_current(&daemon, session)? {
                            socket.close(None).await?;
                            return Ok(RemoteExit::Closed);
                        }
                        // Asides are ephemeral and do not replace the active completion identity.
                        socket.send(Message::Text(serde_json::json!({"v":1,"type":"btw","text":question}).to_string().into())).await?;
                        continue;
                    }
                    Some(Input::StopJobOutput(observer)) => {
                        if access.controller && job_observer_current(&daemon, session, &observer)? {
                            socket.send(Message::Text(serde_json::json!({"v":1,"type":"job_output_stop","observer":observer}).to_string().into())).await?;
                        }
                        continue;
                    }
                    Some(Input::FrameControl(request)) => {
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        let valid = access.controller && current.generation == session.generation
                            && daemon.telemetry.lock().expect("telemetry poisoned")
                                .get(&session.id).filter(|(owner, _)| *owner == session.generation)
                                .and_then(|(_, counters)| counters.frame.as_ref())
                                .is_some_and(|frame| frame.id == request.id) && request.is_valid();
                        if valid {
                            socket.send(Message::Text(serde_json::json!({"v":1,"type":"frame_input", "request":request}).to_string().into())).await?;
                        }
                        continue;
                    },
                    Some(Input::LiveControl(request)) => {
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        if access.controller && current.generation == session.generation && current.can_receive_live_control(request) {
                            socket.send(Message::Text(serde_json::json!({"v":1,"type":"live_control","request":request}).to_string().into())).await?;
                        }
                        continue;
                    }
                    Some(Input::Observe) => continue,
                    Some(Input::RemoteControl(request)) => {
                        let frame_open = daemon.telemetry.lock().expect("telemetry poisoned")
                            .get(&session.id).filter(|(owner, _)| *owner == session.generation)
                            .is_some_and(|(_, counters)| counters.frame.is_some());
                        ensure!((active.is_none() || frame_open) && (request || pending.is_none()), "Cannot release control while input is active or waiting");
                        socket.send(Message::Text(serde_json::json!({"v":1,"type":if request {"request_control"} else {"release_control"}}).to_string().into())).await?;
                        access.requesting = request;
                        if !request { access.controller = false; }
                        access.reason = None;
                        publish_access(&daemon, session, &access)?;
                        continue;
                    }
                    Some(Input::Stop) => {
                        if access.controller && active.is_some() {
                            let cancel = match &active_request {
                                Some(request_id) => serde_json::json!({"v":1,"type":"cancel_prompt","request_id":request_id}),
                                None => serde_json::json!({"v":1,"type":"interrupt"}),
                            };
                            socket.send(Message::Text(cancel.to_string().into())).await?;
                        }
                        socket.close(None).await?;
                        return Ok(RemoteExit::Closed);
                    }
                    Some(Input::Approval { approval, choice }) => {
                        ensure!(access.controller, "Remote approval requires control");
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        if current.generation != session.generation || !current.approvals.iter().any(|pending| pending.id == approval.id && pending.requirement == approval.requirement) {
                            continue;
                        }
                        document_save = None;
                        let stream = current.controls.as_ref().is_some_and(|controls| controls.features.iter()
                            .any(|feature| feature.name == "editor_stream_save" && feature.available));
                        if let Some(frames) = document_save_frames(&approval, &choice, stream)? {
                            document_save = Some((approval, frames));
                        } else {
                            socket.send(Message::Text(remote_answer(&approval, &choice)?.to_string().into())).await?;
                        }
                        continue;
                    }
                    None => {
                        ensure!(active.is_none(), "Remote input closed while a run was active; inspect its session before retrying");
                        socket.close(None).await?;
                        return Ok(RemoteExit::Closed);
                    }
                }
            }
        };
        if frame.is_close() {
            if let Some(instance) = restarting {
                return Ok(RemoteExit::Restart(instance));
            }
            bail!("Remote disconnected; the remote run may still be executing");
        }
        if !frame.is_text() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(frame.to_text()?)?;
        if value["type"] == "edit_rejected"
            && document_save.as_ref().is_some_and(|(approval, _)| {
                value["document_id"].as_str() == Some(approval.id.as_str())
            })
        {
            document_save = None;
        }
        if value["type"] == "restarting" {
            ensure!(
                active.is_none() && pending.is_none() && input.is_empty(),
                "Remote announced restart before completing accepted input; inspect its session before retrying"
            );
            let instance = hello["instance_id"]
                .as_str()
                .context("Remote restart requires a server instance identity")?;
            ensure!(
                value["instance_id"].as_str() == Some(instance),
                "Remote restart identity does not match this connection"
            );
            if restarting.is_none() {
                mark_remote_restarting(&daemon, session)?;
                restarting = Some(instance.to_owned());
                // The server sends bye immediately after the notification;
                // allow bounded socket flushing before waiting for replacement.
                restart_close_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(2));
            }
            continue;
        }
        if let Some(instance) = &restarting {
            ensure!(
                value["type"] == "bye",
                "Unexpected old-server frame after restart notification"
            );
            return Ok(RemoteExit::Restart(instance.clone()));
        }
        ingest_remote_opening(&daemon, session, &value, &mut editor_opening, false)?;
        if value["type"] == "bye" {
            // Fence new daemon submissions before checking for queued input.
            // Receiving after close also waits for outstanding sender permits.
            input.close();
            ensure!(
                active.is_none() && pending.is_none() && input.recv().await.is_none(),
                "Remote closed before completing accepted input; inspect its session before retrying"
            );
            let reason = value["reason"]
                .as_str()
                .unwrap_or("Remote host closed")
                .chars()
                .filter(|character| !character.is_control())
                .take(128)
                .collect::<String>();
            append(&daemon, session, "notice", &reason)?;
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            if telemetry
                .get(&session.id)
                .is_some_and(|(generation, _)| *generation == session.generation)
            {
                telemetry.remove(&session.id);
            }
            drop(telemetry);
            state(&daemon, session, RunState::Idle)?;
            return Ok(RemoteExit::Closed);
        }
        if value["type"] == "control" {
            access.controller = value["controller"]
                .as_bool()
                .context("Invalid control role")?;
            access.requesting = false;
            access.reason = None;
            let frame_open = daemon
                .telemetry
                .lock()
                .expect("telemetry poisoned")
                .get(&session.id)
                .filter(|(owner, _)| *owner == session.generation)
                .is_some_and(|(_, counters)| counters.frame.is_some());
            ensure!(
                access.controller || active.is_none() || frame_open,
                "Control changed during an active input; inspect the host before retrying"
            );
            if access.controller
                && let Some(prompt) = pending.take()
            {
                if !remote_generation_current(&daemon, session)? {
                    socket
                        .send(Message::Text(
                            serde_json::json!({"v":1,"type":"release_control"})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                    socket.close(None).await?;
                    return Ok(RemoteExit::Closed);
                }
                let (payload, request_id) = remote_payload(&prompt, capabilities)?;
                socket
                    .send(Message::Text(payload.to_string().into()))
                    .await?;
                active_request = request_id;
                active = Some(prompt);
                state(&daemon, session, RunState::Running)?;
            }
            access.waiting_input = pending.is_some();
            publish_access(&daemon, session, &access)?;
        }
        if value["type"] == "control_denied" {
            let awaiting_frame_grant = access.requesting
                && daemon
                    .telemetry
                    .lock()
                    .expect("telemetry poisoned")
                    .get(&session.id)
                    .filter(|(owner, _)| *owner == session.generation)
                    .is_some_and(|(_, counters)| counters.frame.is_some());
            ensure!(
                active.is_none() || awaiting_frame_grant,
                "Remote rejected an active input; inspect its session before retrying: {}",
                value["reason"]
            );
            access.requesting = false;
            access.reason = Some(
                value["reason"]
                    .as_str()
                    .unwrap_or("Control denied")
                    .chars()
                    .filter(|character| !character.is_control())
                    .take(128)
                    .collect(),
            );
            publish_access(&daemon, session, &access)?;
        }
        if active
            .as_deref()
            .is_some_and(|prompt| remote_finished(&value, prompt, active_request.as_deref()))
        {
            state(&daemon, session, RunState::Idle)?;
            active = None;
            active_request = None;
        }
    }
}

fn mark_remote_restarting(daemon: &Daemon, session: &Session) -> Result<()> {
    let store = daemon.store.lock().expect("store poisoned");
    ensure!(
        store.session(&session.id)?.generation == session.generation,
        "Remote worker was replaced before restart notification"
    );
    store.engine_restarting(&session.id, session.generation)?;
    store.set_state(&session.id, RunState::Starting, false)?;
    store.append(
        &session.id,
        "notice",
        "Remote engine restarting; reconnecting as an observer.",
    )?;
    let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
    if telemetry
        .get(&session.id)
        .is_some_and(|(generation, _)| *generation == session.generation)
    {
        telemetry.remove(&session.id);
    }
    drop(telemetry);
    let mut contexts = daemon.contexts.lock().expect("contexts poisoned");
    if contexts
        .get(&session.id)
        .is_some_and(|(generation, _)| *generation == session.generation)
    {
        contexts.remove(&session.id);
    }
    drop(contexts);
    daemon
        .artifacts
        .lock()
        .expect("artifacts poisoned")
        .clear(&session.id, session.generation);
    Ok(())
}

fn bind_remote_instance(
    daemon: &Daemon,
    session: &Session,
    hello: &serde_json::Value,
) -> Result<()> {
    let instance = match hello.get("instance_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(instance)) => Some(instance.as_str()),
        _ => bail!("Invalid remote server instance identity"),
    };
    let mut store = daemon.store.lock().expect("store poisoned");
    let changed = store.bind_remote_instance(&session.id, session.generation, instance)?;
    if changed {
        daemon
            .notifications
            .lock()
            .expect("notifications poisoned")
            .clear(&session.id);
        {
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            if telemetry
                .get(&session.id)
                .is_some_and(|(generation, _)| *generation == session.generation)
            {
                telemetry.remove(&session.id);
            }
        }
        {
            let mut contexts = daemon.contexts.lock().expect("contexts poisoned");
            if contexts
                .get(&session.id)
                .is_some_and(|(generation, _)| *generation == session.generation)
            {
                contexts.remove(&session.id);
            }
        }
        daemon
            .artifacts
            .lock()
            .expect("artifacts poisoned")
            .clear(&session.id, session.generation);
    }
    Ok(())
}

async fn publish_focus(
    daemon: &Daemon,
    session: &Session,
    mailbox: &std::path::Path,
) -> Result<()> {
    let focused = daemon.views.lock().expect("view focus poisoned").focused(
        &session.id,
        session.generation,
        std::time::Instant::now(),
    );
    let snapshot = crate::guifocus::Snapshot {
        focused,
        expires_ms: crate::guifocus::now_ms().saturating_add(crate::guifocus::LIFETIME_MS),
    };
    let temporary = mailbox.with_extension("tmp");
    tokio::fs::write(&temporary, serde_json::to_vec(&snapshot)?).await?;
    tokio::fs::rename(temporary, mailbox).await?;
    Ok(())
}

pub(crate) fn job_observer_current(
    daemon: &Daemon,
    session: &Session,
    observer: &str,
) -> Result<bool> {
    if !crate::joboutput::valid_observer(observer)
        || daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&session.id)?
            .generation
            != session.generation
    {
        return Ok(false);
    }
    Ok(daemon.telemetry.lock().expect("telemetry poisoned").get(&session.id)
        .filter(|(owner, _)| *owner == session.generation)
        .and_then(|(_, telemetry)| telemetry.metrics.get("job_output"))
        .is_some_and(|metrics| matches!(metrics, crate::model::EngineMetrics::JobOutput { snapshot } if snapshot.following && snapshot.observer.as_deref() == Some(observer))))
}

fn remote_generation_current(daemon: &Daemon, session: &Session) -> Result<bool> {
    Ok(daemon
        .store
        .lock()
        .expect("store poisoned")
        .session(&session.id)?
        .generation
        == session.generation)
}

fn publish_access(
    daemon: &Daemon,
    session: &Session,
    access: &crate::model::RemoteAccess,
) -> Result<()> {
    let store = daemon.store.lock().expect("store poisoned");
    if store.session(&session.id)?.generation == session.generation {
        let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
        let entry = telemetry
            .entry(session.id.clone())
            .or_insert_with(|| (session.generation, Default::default()));
        if entry.0 == session.generation {
            entry.1.remote_access = Some(access.clone());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent advertised wire capabilities, not state-machine modes"
)]
struct RemoteInputCaps {
    commands: bool,
    prompts: bool,
    identified_commands: bool,
    ownership_fencing: bool,
}

impl RemoteInputCaps {
    fn from_hello(hello: &serde_json::Value) -> Self {
        Self {
            commands: hello["command_completion"] == true,
            prompts: hello["prompt_completion"] == true,
            identified_commands: hello["command_request_completion"] == true,
            ownership_fencing: hello["ownership_fencing"] == true,
        }
    }
}

fn remote_payload(
    prompt: &str,
    capabilities: RemoteInputCaps,
) -> Result<(serde_json::Value, Option<String>)> {
    remote_input(prompt, capabilities.commands)?;
    ensure!(
        capabilities.ownership_fencing,
        "Remote Plank lacks input ownership fencing; rebuild the host before submitting this input"
    );
    let command = prompt.trim_start().starts_with('/');
    ensure!(
        if command {
            capabilities.identified_commands
        } else {
            capabilities.prompts
        },
        "Remote Plank lacks identified input completion; rebuild the host before submitting this input"
    );
    let request_id = uuid::Uuid::new_v4().to_string();
    Ok((
        serde_json::json!({"v":1,"type":if command {"submit_command"} else {"submit_prompt"},"request_id":request_id,"text":prompt}),
        Some(request_id),
    ))
}

fn remote_input(prompt: &str, completion: bool) -> Result<serde_json::Value> {
    let command = prompt.trim_start().starts_with('/');
    ensure!(
        !command || completion,
        "Remote Plank does not support command completion; rebuild the remote engine before using slash controls"
    );
    Ok(serde_json::json!({"v":1,"type":if command {"command"} else {"prompt"},"text":prompt}))
}

fn document_save_frames(
    approval: &crate::model::PendingApproval,
    choice: &str,
    available: bool,
) -> Result<Option<crate::documenttransfer::Sender<'static>>> {
    if !available || approval.requirement["kind"] != "plank_editor" {
        return Ok(None);
    }
    let text: Option<String> = serde_json::from_str(choice)?;
    let Some(text) = text.filter(|text| text.len() > crate::documenttransfer::CHUNK_BYTES) else {
        return Ok(None);
    };
    ensure!(
        text.len() <= crate::documenttransfer::DOCUMENT_BYTES,
        "Document exceeds the editing limit"
    );
    crate::documenttransfer::Sender::owned(uuid::Uuid::new_v4().to_string(), text)
        .map(Some)
        .map_err(anyhow::Error::msg)
}

fn remote_answer(
    approval: &crate::model::PendingApproval,
    choice: &str,
) -> Result<serde_json::Value> {
    if approval.requirement["kind"] == "plank_editor" {
        let text: Option<String> = serde_json::from_str(choice)?;
        ensure!(
            text.as_ref().is_none_or(|text| text.len() <= 256 * 1024),
            "Document exceeds the editing limit"
        );
        return Ok(
            serde_json::json!({"v":1, "type":"edit", "document_id":approval.id, "text":text}),
        );
    }
    ensure!(
        approval.requirement["kind"] == "plank_question",
        "Unsupported remote approval protocol"
    );
    ensure!(
        approval.accepts_choice(choice),
        "Invalid remote question choice"
    );
    Ok(serde_json::json!({"v":1,"type":"answer","question_id":approval.id,"answer":choice}))
}

fn remote_finished(value: &serde_json::Value, prompt: &str, request_id: Option<&str>) -> bool {
    if prompt.trim_start().starts_with('/') {
        match request_id {
            Some(id) => {
                value["type"] == "command_request_done" && value["request_id"].as_str() == Some(id)
            }
            None => {
                value["type"] == "command_done" && value["text"].as_str() == Some(prompt.trim())
            }
        }
    } else {
        request_id.is_some()
            && value["type"] == "prompt_done"
            && value["request_id"].as_str() == request_id
    }
}

fn ingest_local_notification(
    daemon: &Daemon,
    session: &Session,
    value: &serde_json::Value,
) -> Result<()> {
    let now = crate::notifications::now_ms();
    let Some(notice) = crate::notifications::Notification::from_remote(
        &session.id,
        session.generation,
        value,
        now,
    ) else {
        return Ok(());
    };
    let store = daemon.store.lock().expect("store poisoned");
    let current = store.session(&session.id)?;
    if current.generation == session.generation && !current.archived && !current.recovery_required {
        daemon
            .notifications
            .lock()
            .expect("notifications poisoned")
            .push(notice, now);
    }
    Ok(())
}

fn ingest_remote_notification(
    daemon: &Daemon,
    session: &Session,
    value: &serde_json::Value,
    live: bool,
) -> Result<()> {
    let now = crate::notifications::now_ms();
    let Some(notice) = crate::notifications::Notification::from_remote(
        &session.id,
        session.generation,
        value,
        now,
    ) else {
        return Ok(());
    };
    let Some(sequence) = value["id"].as_i64() else {
        return Ok(());
    };
    let store = daemon.store.lock().expect("store poisoned");
    if store.record_remote_event(&session.id, session.generation, sequence)? && live {
        daemon
            .notifications
            .lock()
            .expect("notifications poisoned")
            .push(notice, now);
    }
    Ok(())
}

fn ingest_remote_opening(
    daemon: &Daemon,
    session: &Session,
    value: &serde_json::Value,
    opening: &mut EditorOpeningInbox,
    replay: bool,
) -> Result<()> {
    if value["type"] == "replay" {
        ensure!(!replay, "Nested replay envelopes are invalid");
        return ingest_remote_opening(daemon, session, &value["entry"], opening, true);
    }
    if let Some(entries) = value["scrollback"].as_array() {
        ensure!(!replay, "Nested replay snapshots are invalid");
        for entry in entries {
            ingest_remote_opening(daemon, session, entry, opening, true)?;
        }
        return Ok(());
    }
    if value["type"] == "notify" && replay {
        return ingest_remote_notification(daemon, session, value, false);
    }
    if value["type"] == "editor_stream" {
        if let Some(editor) = opening.feed(&value["request"].to_string(), session.generation)? {
            let store = daemon.store.lock().expect("store poisoned");
            if store.session(&session.id)?.generation == session.generation {
                store.approval(&session.id, session.generation, editor)?;
            }
        }
        return Ok(());
    }
    match value["type"].as_str() {
        Some("editor_done") => {
            if let Some(id) = value["document_id"].as_str() {
                opening.cancel(id);
            }
        }
        Some("editor" | "question" | "reset" | "restarting" | "bye") => {
            *opening = EditorOpeningInbox::default();
        }
        _ => {}
    }
    ingest_remote(daemon, session, value)
}

fn ingest_remote(daemon: &Daemon, session: &Session, value: &serde_json::Value) -> Result<()> {
    if value["type"] == "notify" {
        return ingest_remote_notification(daemon, session, value, true);
    }
    if value["type"] == "live_control_rejected" {
        let reason: String = value["reason"]
            .as_str()
            .unwrap_or("Host refused live setting")
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect();
        return append(
            daemon,
            session,
            "system",
            &format!("Live setting rejected: {reason}"),
        );
    }
    if value["type"] == "job_output_rejected" {
        let reason: String = value["reason"]
            .as_str()
            .unwrap_or("stale output observer")
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect();
        return append(
            daemon,
            session,
            "system",
            &format!("Output stop rejected: {reason}"),
        );
    }
    if value["type"] == "frame" && value.get("frame_id").is_some() {
        if !value["snapshot"].is_null() {
            ensure!(
                value["snapshot"]["id"] == value["frame_id"],
                "Frame snapshot identity mismatch"
            );
        }
        let mut frame = value.clone();
        frame["id"] = value["frame_id"].clone();
        return ingest_telemetry(daemon, session, &frame);
    }
    if value["type"] == "frame_rejected" {
        let reason: String = value["reason"]
            .as_str()
            .unwrap_or("stale frame input")
            .chars()
            .filter(|character| !character.is_control())
            .take(512)
            .collect();
        return append(
            daemon,
            session,
            "system",
            &format!("Frame input rejected: {reason}"),
        );
    }
    if value["type"] == "aside" {
        return ingest_telemetry(daemon, session, value);
    }
    if matches!(
        value["type"].as_str(),
        Some(
            "status"
                | "buddy"
                | "tasks"
                | "jobs"
                | "sessions"
                | "config"
                | "history"
                | "extensions"
                | "cache"
                | "downloads"
                | "metrics"
                | "host"
                | "reset"
                | "artifact"
                | "frame"
        )
    ) {
        return ingest_telemetry(daemon, session, value);
    }
    if let Some(replay) = value["scrollback"].as_array() {
        for entry in replay {
            if entry["type"] == "notify" {
                ingest_remote_notification(daemon, session, entry, false)?;
            } else {
                ingest_remote(daemon, session, entry)?;
            }
        }
        return Ok(());
    }
    if value["type"] == "question" {
        let approval = plank_question(&value["request"].to_string(), session.generation)?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            store.approval(&session.id, session.generation, approval)?;
        }
        return Ok(());
    }
    if value["type"] == "editor" {
        let editor = plank_editor(&value["request"].to_string(), session.generation)?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            store.approval(&session.id, session.generation, editor)?;
        }
        return Ok(());
    }
    if value["type"] == "editor_done" {
        let id = value["document_id"]
            .as_str()
            .context("Remote editor completion is missing its ID")?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            store.resolve_approval(&session.id, session.generation, id)?;
        }
        return Ok(());
    }
    if value["type"] == "question_done" {
        let id = value["question_id"]
            .as_str()
            .context("Remote question completion is missing its ID")?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            store.resolve_approval(&session.id, session.generation, id)?;
        }
        return Ok(());
    }
    if value["type"] == "answer_rejected" || value["type"] == "edit_rejected" {
        append(
            daemon,
            session,
            "error",
            value["reason"]
                .as_str()
                .unwrap_or("Remote rejected the answer"),
        )?;
        return Ok(());
    }
    if value["type"] == "controls" {
        let controls = serde_json::from_value(value["controls"].clone())?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            store.controls(&session.id, session.generation, controls)?;
        }
        return Ok(());
    }
    let role = match value["type"].as_str().unwrap_or("") {
        "visible" | "plain" | "markdown" => "assistant",
        "think" => "thinking",
        "tool" => "tool",
        "error" => "error",
        "dim" | "system_status" => "system",
        _ => return Ok(()),
    };
    if let Some(text) = value["text"].as_str() {
        let mut store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation != session.generation {
            return Ok(());
        }
        if let Some(sequence) = value["id"].as_i64() {
            store.append_remote(&session.id, sequence, role, text)?;
        } else {
            store.append(&session.id, role, text)?;
        }
    }
    Ok(())
}

fn ingest_telemetry(daemon: &Daemon, session: &Session, value: &serde_json::Value) -> Result<()> {
    if value["type"] == "buddy" {
        let frame: Option<crate::buddyframe::Frame> = serde_json::from_value(
            value
                .get("snapshot")
                .context("Buddy snapshot missing")?
                .clone(),
        )?;
        ensure!(
            frame
                .as_ref()
                .is_none_or(crate::buddyframe::Frame::is_valid),
            "Invalid buddy frame"
        );
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            let entry = telemetry
                .entry(session.id.clone())
                .or_insert_with(|| (session.generation, Default::default()));
            if entry.0 != session.generation {
                *entry = (session.generation, Default::default());
            }
            entry.1.buddy = frame;
        }
        return Ok(());
    }
    if value["type"] == "frame" {
        let payload = value.get("snapshot").context("Frame snapshot is missing")?;
        let frame: Option<crate::frame::Snapshot> = serde_json::from_value(payload.clone())?;
        ensure!(
            frame.as_ref().is_none_or(crate::frame::Snapshot::is_valid),
            "Invalid graphical grid"
        );
        let store = daemon.store.lock().expect("store poisoned");
        let current = store.session(&session.id)?;
        if current.generation == session.generation {
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            if frame.is_none() {
                let id = value["id"]
                    .as_str()
                    .context("Frame close identity is missing")?;
                ensure!(
                    crate::frame::Request {
                        id: id.into(),
                        input: crate::frame::Input::Close
                    }
                    .is_valid(),
                    "Invalid frame close identity"
                );
                if let Some((owner, counters)) = telemetry.get_mut(&session.id)
                    && *owner == session.generation
                    && counters.frame.as_ref().is_some_and(|frame| frame.id == id)
                {
                    counters.frame = None;
                }
                return Ok(());
            }
            let changed = telemetry.get(&session.id).is_none_or(|(owner, counters)| {
                *owner != session.generation
                    || counters.frame.as_ref().map(|frame| &frame.id)
                        != frame.as_ref().map(|frame| &frame.id)
            });
            let entry = telemetry
                .entry(session.id.clone())
                .or_insert_with(|| (session.generation, Default::default()));
            if entry.0 != session.generation {
                *entry = (session.generation, Default::default());
            }
            entry.1.frame = frame;
            if changed {
                store.request_attention(&session.id)?;
            }
        }
        return Ok(());
    }
    if value["type"] == "aside" {
        let role = match value["role"].as_str() {
            Some("answer") => "aside",
            Some("thinking") => "aside_thinking",
            Some("tool") => "aside_tool",
            Some("error") => "aside_error",
            Some("notice") => "aside_notice",
            _ => bail!("Invalid aside role"),
        };
        let text = value["text"].as_str().context("Aside text is missing")?;
        ensure!(text.len() <= 256 * 1024, "Aside frame is too large");
        let mut store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
            if let Some(sequence) = value["id"].as_i64() {
                store.append_remote(&session.id, sequence, role, text)?;
            } else {
                store.append(&session.id, role, text)?;
            }
        }
        return Ok(());
    }
    if value["type"] == "artifact" {
        let event: crate::artifacts::ArtifactEvent =
            serde_json::from_value(value["event"].clone())?;
        let store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation != session.generation {
            return Ok(());
        }
        let mut artifacts = daemon.artifacts.lock().expect("artifacts poisoned");
        let retirement = match &event {
            crate::artifacts::ArtifactEvent::Abort { id, reason }
                if artifacts.owns_pending(&session.id, session.generation, id) =>
            {
                Some(reason.clone())
            }
            _ => None,
        };
        let completed = artifacts.ingest(&session.id, session.generation, event)?;
        let inventory = completed.as_ref().map(|_| {
            (
                artifacts.metadata(&session.id, session.generation),
                artifacts.retired(&session.id, session.generation),
            )
        });
        drop(artifacts);
        if let Some(reason) = retirement {
            store.append(
                &session.id,
                "system",
                &format!("Export transfer incomplete: {reason}. Request /export again."),
            )?;
        }
        if let Some((inventory, retired)) = inventory {
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            let entry = telemetry
                .entry(session.id.clone())
                .or_insert_with(|| (session.generation, Default::default()));
            if entry.0 != session.generation {
                *entry = (session.generation, Default::default());
            }
            entry.1.replace_artifacts(inventory, retired);
        }
        return Ok(());
    }
    if value["type"] == "reset" {
        let mut store = daemon.store.lock().expect("store poisoned");
        if store.conversation_replaced(&session.id, session.generation, value["id"].as_i64())? {
            daemon
                .notifications
                .lock()
                .expect("notifications poisoned")
                .clear(&session.id);
            daemon
                .artifacts
                .lock()
                .expect("artifacts poisoned")
                .clear(&session.id, session.generation);
            daemon
                .telemetry
                .lock()
                .expect("telemetry poisoned")
                .remove(&session.id);
        }
        return Ok(());
    }
    ensure!(
        matches!(
            value["type"].as_str(),
            Some(
                "status"
                    | "tasks"
                    | "jobs"
                    | "sessions"
                    | "config"
                    | "history"
                    | "extensions"
                    | "cache"
                    | "downloads"
                    | "metrics"
                    | "host"
            )
        ),
        "Unknown telemetry frame"
    );
    let status = if value["type"] == "status" {
        let status: crate::model::EngineStatus = serde_json::from_value(value["status"].clone())?;
        ensure!(
            matches!(
                status.state.as_str(),
                "idle" | "prefill" | "generating" | "compacting" | "saving" | "error" | "stopped"
            ) && status.think.len() <= 16
                && !status.think.chars().any(char::is_control)
                && status.gen_tps.is_finite()
                && status.gen_tps >= 0.0
                && status.elapsed_secs.is_finite()
                && status.elapsed_secs >= 0.0,
            "Invalid engine status"
        );
        Some(status)
    } else {
        None
    };
    let tasks = if value["type"] == "tasks" {
        let completed = value["completed"]
            .as_u64()
            .context("Invalid completed task count")?;
        let total = value["total"]
            .as_u64()
            .context("Invalid total task count")?;
        ensure!(completed <= total, "Invalid task counts");
        Some((completed, total))
    } else {
        None
    };
    let task_rows = if let Some((completed, total)) = tasks {
        let rows = value
            .get("rows")
            .map(|rows| serde_json::from_value::<Vec<crate::model::EngineTask>>(rows.clone()))
            .transpose()?;
        if let Some(rows) = &rows {
            let visible_completed =
                rows.iter().filter(|row| row.status == "completed").count() as u64;
            ensure!(
                rows.len() as u64 == total.min(64)
                    && rows.iter().all(crate::model::EngineTask::is_valid)
                    && rows
                        .iter()
                        .enumerate()
                        .all(|(index, row)| rows[..index].iter().all(|other| other.id != row.id))
                    && visible_completed <= completed
                    && completed - visible_completed <= total - rows.len() as u64,
                "Invalid task rows"
            );
        }
        rows
    } else {
        None
    };
    let goal_label = if tasks.is_some() {
        let goal: Option<String> = serde_json::from_value(
            value
                .get("goal")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )?;
        ensure!(
            goal.as_deref()
                .is_none_or(|text| text.len() <= 1024 && !text.chars().any(char::is_control)),
            "Invalid goal label"
        );
        goal
    } else {
        None
    };
    let store = daemon.store.lock().expect("store poisoned");
    let saved_sessions = if value["type"] == "sessions" {
        let sessions: crate::model::EngineSavedSessions =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(sessions.is_valid(), "Invalid saved-session snapshot");
        Some(sessions)
    } else {
        None
    };
    let config = if value["type"] == "config" {
        let config: crate::model::EngineConfig = serde_json::from_value(value["snapshot"].clone())?;
        ensure!(config.is_valid(), "Invalid settings snapshot");
        Some(config)
    } else {
        None
    };
    let history = if value["type"] == "history" {
        let history: crate::model::EngineHistory =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(history.is_valid(), "Invalid conversation history snapshot");
        Some(history)
    } else {
        None
    };
    let host = if value["type"] == "host" {
        let snapshot: crate::model::EngineHost = serde_json::from_value(value["snapshot"].clone())?;
        ensure!(snapshot.is_valid(), "Invalid remote-host snapshot");
        Some(snapshot)
    } else {
        None
    };
    let metrics = if value["type"] == "metrics" {
        let snapshot: crate::model::EngineMetrics =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(snapshot.is_valid(), "Invalid measurement snapshot");
        Some(snapshot)
    } else {
        None
    };
    let downloads = if value["type"] == "downloads" {
        let snapshot: crate::model::EngineDownloads =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(snapshot.is_valid(), "Invalid download snapshot");
        Some(snapshot)
    } else {
        None
    };
    let cache = if value["type"] == "cache" {
        let snapshot: crate::model::EngineCache =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(snapshot.is_valid(), "Invalid cache snapshot");
        Some(snapshot)
    } else {
        None
    };
    let extensions = if value["type"] == "extensions" {
        let snapshot: crate::model::EngineExtensions =
            serde_json::from_value(value["snapshot"].clone())?;
        ensure!(snapshot.is_valid(), "Invalid extension inventory");
        Some(snapshot)
    } else {
        None
    };
    let jobs = if value["type"] == "jobs" {
        let jobs: crate::model::EngineJobs = serde_json::from_value(value["snapshot"].clone())?;
        validate_jobs(&jobs)?;
        Some(jobs)
    } else {
        None
    };
    let report = if value["type"] == "tasks" {
        match value.get("report") {
            Some(report) => {
                let report = report.as_str().context("Invalid task report")?;
                // Match the document size ceiling to bound each live snapshot.
                ensure!(report.len() <= 256 * 1024, "Task report is too large");
                Some(report.to_owned())
            }
            None => None,
        }
    } else {
        None
    };
    if store.session(&session.id)?.generation != session.generation {
        return Ok(());
    }
    let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
    let entry = telemetry.entry(session.id.clone()).or_default();
    if entry.0 != session.generation {
        *entry = (session.generation, crate::model::EngineTelemetry::default());
    }
    if let Some(status) = status {
        entry.1.status = Some(status);
    }
    if let Some(tasks) = tasks {
        entry.1.tasks = Some(tasks);
        entry.1.task_report = report;
        entry.1.task_rows = task_rows;
        entry.1.goal_label = goal_label;
    }
    if let Some(jobs) = jobs {
        entry.1.jobs = Some(jobs);
    }
    if let Some(saved_sessions) = saved_sessions {
        entry.1.saved_sessions = Some(saved_sessions);
    }
    if let Some(config) = config {
        entry.1.config = Some(config);
    }
    if let Some(history) = history {
        if history.kind == "checkpoints" {
            entry.1.checkpoints = Some(history);
        } else {
            entry.1.branches = Some(history);
        }
    }
    if let Some(snapshot) = host {
        entry.1.host = Some(snapshot);
    }
    if let Some(snapshot) = metrics {
        entry.1.metrics.insert(snapshot.kind().to_owned(), snapshot);
    }
    if let Some(snapshot) = downloads {
        entry.1.downloads = Some(snapshot);
    }
    if let Some(snapshot) = cache {
        entry.1.cache = Some(snapshot);
    }
    if let Some(snapshot) = extensions {
        entry.1.extensions.insert(snapshot.kind.clone(), snapshot);
    }
    Ok(())
}

fn validate_jobs(snapshot: &crate::model::EngineJobs) -> Result<()> {
    ensure!(
        snapshot.running <= snapshot.total
            && snapshot.jobs.len() <= 64
            && snapshot.jobs.len() as u64 <= snapshot.total,
        "Invalid job counters"
    );
    let mut ids = std::collections::HashSet::new();
    for job in &snapshot.jobs {
        ensure!(
            job.id > 0
                && job.pid > 0
                && ids.insert(job.id)
                && job.elapsed_secs.is_finite()
                && job.elapsed_secs >= 0.0
                && job.path.len() <= 4096
                && match job.state.as_str() {
                    "running" => job.exit_code.is_none(),
                    "done" | "timed_out" => job.exit_code.is_some(),
                    _ => false,
                },
            "Invalid job snapshot"
        );
    }
    let visible_running = snapshot
        .jobs
        .iter()
        .filter(|job| job.state == "running")
        .count() as u64;
    ensure!(
        visible_running <= snapshot.running
            && snapshot.running - visible_running <= snapshot.total - snapshot.jobs.len() as u64
            && (snapshot.jobs.len() as u64 != snapshot.total
                || visible_running == snapshot.running),
        "Inconsistent running job count"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::decode_utf8;

    #[test]
    fn editor_opening_streams_publish_only_complete_verified_documents() -> anyhow::Result<()> {
        let text = "\u{0}🦀\\\r\n".repeat(20_000);
        let title = "\u{0}".repeat(4096);
        let mut inbox = super::EditorOpeningInbox::default();
        let mut complete = None;
        for frame in crate::documenttransfer::Sender::new("open-1", &text).unwrap() {
            let end = matches!(frame, crate::documenttransfer::Frame::End { .. });
            let value = serde_json::json!({"id":"editor-1","title":title,"frame":frame});
            let encoded = value.to_string();
            assert!(encoded.len() <= super::EDITOR_STREAM_FRAME_BYTES);
            complete = inbox.feed(&encoded, 17)?;
            assert_eq!(complete.is_some(), end);
        }
        let complete = complete.unwrap();
        assert_eq!(complete.arguments, text);
        assert_eq!(complete.tool, title);
        assert_eq!(complete.requirement["generation"], 17);
        assert_eq!(complete.requirement["kind"], "plank_editor");
        assert!(inbox.feed(&serde_json::json!({"id":"editor-1","title":title,"frame":{"operation":"end","transfer":"open-1"}}).to_string(), 17)?.is_none());
        Ok(())
    }

    #[test]
    fn editor_opening_streams_accept_the_full_document_limit() -> anyhow::Result<()> {
        // Multi-byte pattern repeated to exactly the 32 MiB document limit.
        let pattern = "\u{1f980}\u{0}\\\r\n";
        let text = pattern.repeat(crate::documenttransfer::DOCUMENT_BYTES / pattern.len());
        assert_eq!(text.len(), crate::documenttransfer::DOCUMENT_BYTES);
        let mut inbox = super::EditorOpeningInbox::default();
        let mut complete = None;
        for frame in crate::documenttransfer::Sender::new("open-large", &text).unwrap() {
            let end = matches!(frame, crate::documenttransfer::Frame::End { .. });
            let encoded =
                serde_json::json!({"id":"editor-large","title":"Large","frame":frame}).to_string();
            assert!(encoded.len() <= super::EDITOR_STREAM_FRAME_BYTES);
            complete = inbox.feed(&encoded, 3)?;
            assert_eq!(complete.is_some(), end);
        }
        assert_eq!(complete.unwrap().arguments, text);
        Ok(())
    }

    #[test]
    fn editor_opening_streams_fence_replaced_cancelled_and_invalid_attempts() -> anyhow::Result<()>
    {
        let body = "private draft";
        let frames: Vec<_> = crate::documenttransfer::Sender::new("open-1", body)
            .unwrap()
            .collect();
        let wire = |id, title, frame: &crate::documenttransfer::Frame| {
            serde_json::json!({"id":id,"title":title,"frame":frame}).to_string()
        };
        let mut inbox = super::EditorOpeningInbox::default();
        inbox.feed(&wire("old", "Title", &frames[0]), 7)?;
        inbox.cancel("old");
        assert!(inbox.feed(&wire("old", "Title", &frames[2]), 7)?.is_none());
        inbox.feed(&wire("current", "Title", &frames[0]), 7)?;
        inbox.cancel("old");
        assert!(inbox.feed(&wire("old", "Title", &frames[1]), 7)?.is_none());
        assert!(
            inbox
                .feed(&wire("current", "Changed", &frames[1]), 7)
                .is_err()
        );
        assert!(
            inbox
                .feed(&wire("current", "Title", &frames[2]), 7)
                .is_err()
        );
        for frame in &frames {
            let result = inbox.feed(&wire("current", "Title", frame), 7)?;
            if let Some(approval) = result {
                assert_eq!(approval.arguments, body);
            }
        }
        let oversized = crate::documenttransfer::Frame::Begin {
            transfer: "oversized".into(),
            total: crate::documenttransfer::DOCUMENT_BYTES + 1,
            sha256: "0".repeat(64),
        };
        assert!(inbox.feed(&wire("bad", "Title", &oversized), 7).is_err());
        Ok(())
    }

    #[test]
    fn remote_editor_openings_wait_for_complete_text_and_retire_closed_attempts()
    -> anyhow::Result<()> {
        for replay in [false, true] {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let body = "🦀\u{0}\\\r\n".repeat(20_000);
            let frames: Vec<_> = crate::documenttransfer::Sender::new("opening-1", &body)
                .unwrap()
                .collect();
            let wire = |frame: &crate::documenttransfer::Frame| {
                let value = serde_json::json!({"type":"editor_stream","id":9,"request":{"id":"editor-1","title":"File","frame":frame}});
                if replay {
                    serde_json::json!({"type":"replay","entry":value})
                } else {
                    value
                }
            };
            let mut opening = super::EditorOpeningInbox::default();
            for frame in &frames[..frames.len() - 1] {
                super::ingest_remote_opening(&daemon, &session, &wire(frame), &mut opening, false)?;
                assert!(
                    daemon.snapshot(Some(&session.id), 0)?.sessions[0]
                        .approvals
                        .is_empty()
                );
            }
            super::ingest_remote_opening(
                &daemon,
                &session,
                &wire(frames.last().unwrap()),
                &mut opening,
                false,
            )?;
            let current = daemon.store.lock().unwrap().session(&session.id)?;
            assert!(
                daemon.store.lock().unwrap().document_chunk(
                    &session.id,
                    &current.approvals[0].id,
                    &current.approvals[0].requirement,
                    0
                )? == body.as_bytes(),
                "Opening must preserve all document bytes"
            );
            let closed = serde_json::json!({"type":"editor_done","document_id":"editor-1"});
            super::ingest_remote_opening(&daemon, &session, &closed, &mut opening, false)?;
            super::ingest_remote_opening(
                &daemon,
                &session,
                &wire(&frames[0]),
                &mut opening,
                false,
            )?;
            super::ingest_remote_opening(&daemon, &session, &closed, &mut opening, false)?;
            for frame in &frames[1..] {
                super::ingest_remote_opening(&daemon, &session, &wire(frame), &mut opening, false)?;
            }
            assert!(
                daemon.snapshot(Some(&session.id), 0)?.sessions[0]
                    .approvals
                    .is_empty()
            );
            let nested = serde_json::json!({"type":"replay","entry":{"type":"replay","entry":wire(&frames[0])}});
            assert!(
                super::ingest_remote_opening(&daemon, &session, &nested, &mut opening, false)
                    .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn console_writes_preserve_partial_frame_order_and_bound_stalled_input() -> anyhow::Result<()> {
        let mut writes = super::ConsoleWrites::default();
        let first = serde_json::json!({"type":"dialog_reply","id":"e1","text":"🦀\r\n\"\\"});
        let second =
            serde_json::json!({"type":"live_control","request":{"control":"hints","enabled":true}});
        writes.json(&first)?;
        let mut output = writes.front()[..3].to_vec();
        writes.advance(3)?;
        writes.json(&second)?;
        while !writes.is_empty() {
            let count = writes.front().len().min(7);
            output.extend_from_slice(&writes.front()[..count]);
            writes.advance(count)?;
        }
        let decoded: Vec<serde_json::Value> = std::str::from_utf8(&output)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(decoded, vec![first, second]);
        assert_eq!(writes.bytes, 0);
        assert!(writes.advance(0).is_err());
        assert!(writes.push(vec![b'x'; 2 * 1024 * 1024]).is_err());
        writes.push(vec![b'x'; 2 * 1024 * 1024 - 1])?;
        writes.advance(1)?;
        writes.push(vec![b'y'; 2 * 1024 * 1024 - 1])?;
        assert!(
            writes.push(vec![]).is_err(),
            "retained prefixes must count toward the bound"
        );
        Ok(())
    }

    #[tokio::test]
    async fn remote_restart_bounds_missing_bye_and_never_sends_input_after_notification()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for submit in [false, true] {
                let (daemon, session, _directory) = remote_restart_fixture()?;
                let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
                let server = tokio::spawn(async move {
                    let mut socket = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
                    socket.next().await.unwrap()?;
                    for frame in [
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"instance_id":"old-instance"}),
                        serde_json::json!({"type":"snapshot","scrollback":[]}),
                        serde_json::json!({"type":"restarting","instance_id":"old-instance"}),
                    ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                    assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))), "retiring connections cannot receive new input");
                    Ok::<_, anyhow::Error>(())
                });
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                let worker_daemon = std::sync::Arc::clone(&daemon);
                let worker_session = session.clone();
                let worker = tokio::spawn(async move {
                    let socket = WebSocketStream::from_raw_socket(client_stream, Role::Client, None).await;
                    super::remote_connection(worker_daemon, &worker_session, socket, "fixture", None, &mut rx, None).await
                });
                loop {
                    if daemon.snapshot(Some(&session.id), 0)?.sessions[0].progress.is_some() { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                if submit { tx.send(super::Input::Prompt("Never send to retiring host".into())).await?; }
                let result = worker.await?;
                if submit { assert!(result.unwrap_err().to_string().contains("not sent")); }
                else { assert_eq!(result?, super::RemoteExit::Restart("old-instance".into())); }
                server.await??;
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_restart_rejects_unfinished_commands_and_mismatched_notifications()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for (completed, instance, expected) in [(false, "old-instance", "before completing"), (true, "wrong-instance", "does not match")] {
                let (daemon, session, _directory) = remote_restart_fixture()?;
                let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
                let server = tokio::spawn(async move {
                    let mut socket = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
                    socket.next().await.unwrap()?;
                    for frame in [
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":true,"instance_id":"old-instance","command_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                        serde_json::json!({"type":"snapshot","scrollback":[]}),
                    ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                    let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    if completed { socket.send(Message::Text(serde_json::json!({"type":"command_request_done","request_id":command["request_id"]}).to_string().into())).await?; }
                    socket.send(Message::Text(serde_json::json!({"type":"restarting","instance_id":instance}).to_string().into())).await?;
                    let _ = socket.next().await;
                    Ok::<_, anyhow::Error>(())
                });
                let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
                let socket = WebSocketStream::from_raw_socket(client_stream, Role::Client, None).await;
                let error = super::remote_connection(std::sync::Arc::clone(&daemon), &session, socket, "fixture", Some("/engines"), &mut rx, None).await.unwrap_err();
                assert!(error.to_string().contains(expected), "{error}");
                server.await??;
                assert!(daemon.snapshot(Some(&session.id), 0)?.sessions[0].progress.is_none());
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_restart_waits_for_changed_identity_before_ingesting_replay()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            super::bind_remote_instance(&daemon, &session, &serde_json::json!({"instance_id":"old-instance"}))?;
            daemon.store.lock().unwrap().append_remote(&session.id, 1, "assistant", "Keep old replay identity")?;
            let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
            let server = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
                let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["type"], "observe_auth");
                socket.send(Message::Text(serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"instance_id":"old-instance"}).to_string().into())).await?;
                assert!(!matches!(socket.next().await, Some(Ok(Message::Text(_)))), "old instances cannot receive commands");
                Ok::<_, anyhow::Error>(())
            });
            let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
            let socket = WebSocketStream::from_raw_socket(client_stream, Role::Client, None).await;
            assert_eq!(super::remote_connection(std::sync::Arc::clone(&daemon), &session, socket, "fixture", None, &mut rx, Some("old-instance")).await?, super::RemoteExit::AwaitReplacement);
            server.await??;
            daemon.store.lock().unwrap().append_remote(&session.id, 1, "assistant", "Must stay deduplicated")?;
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            assert_eq!(snapshot.entries.iter().filter(|entry| entry.role == "assistant").map(|entry| entry.text.as_str()).collect::<Vec<_>>(), ["Keep old replay identity"]);
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_restart_retry_accepts_stop_refuses_queued_input_and_observes_deadline()
    -> anyhow::Result<()> {
        let (daemon, session, _directory) = remote_restart_fixture()?;
        let mut connect = || {
            std::future::pending::<
                anyhow::Result<tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>>,
            >()
        };
        for stop in [true, false] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            tx.send(if stop {
                super::Input::Stop
            } else {
                super::Input::Prompt("Never replay".into())
            })
            .await?;
            let result = super::reconnect_remote(
                &daemon,
                &session,
                &mut rx,
                &mut connect,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await;
            if stop {
                assert!(result?.is_none());
            } else {
                assert!(result.unwrap_err().to_string().contains("not sent"));
            }
        }
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let error = super::reconnect_remote(
            &daemon,
            &session,
            &mut rx,
            &mut connect,
            tokio::time::Instant::now() + std::time::Duration::from_millis(20),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("did not reopen"));
        Ok(())
    }

    fn remote_restart_fixture() -> anyhow::Result<(
        std::sync::Arc<crate::daemon::Daemon>,
        crate::model::Session,
        tempfile::TempDir,
    )> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().unwrap();
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        Ok((daemon, session, directory))
    }

    #[tokio::test]
    async fn remote_notifications_ignore_replay_and_preserve_conversation_state()
    -> anyhow::Result<()> {
        let (daemon, session, _directory) = remote_restart_fixture()?;
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"original"}),
        )?;
        let before = daemon.snapshot(Some(&session.id), 0)?;
        let notice = |id| serde_json::json!({"type":"notify","id":id,"title":"Done","body":"Turn finished","mode":"unfocused"});
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"snapshot","scrollback":[notice(10)]}),
        )?;
        super::ingest_remote(&daemon, &session, &notice(10))?;
        assert!(daemon.snapshot(None, 0)?.notifications.is_empty());
        super::ingest_remote(&daemon, &session, &notice(11))?;
        super::ingest_remote(&daemon, &session, &notice(11))?;
        let after = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(after.notifications.len(), 1);
        assert_eq!(after.notifications[0].session_id, session.id);
        assert_eq!(
            serde_json::to_value(&after.sessions)?,
            serde_json::to_value(&before.sessions)?
        );
        assert_eq!(
            serde_json::to_value(&after.entries)?,
            serde_json::to_value(&before.entries)?
        );
        let mut stale = session.clone();
        stale.generation += 1;
        super::ingest_remote(&daemon, &stale, &notice(12))?;
        let mut invalid = notice(13);
        invalid["mode"] = serde_json::Value::Null;
        super::ingest_remote(&daemon, &session, &invalid)?;
        invalid["mode"] = "unknown".into();
        super::ingest_remote(&daemon, &session, &invalid)?;
        invalid["mode"] = "always".into();
        invalid["body"] = "x".repeat(4097).into();
        super::ingest_remote(&daemon, &session, &invalid)?;
        assert_eq!(daemon.snapshot(None, 0)?.notifications.len(), 1);
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"reset","id":14}),
        )?;
        assert!(daemon.snapshot(None, 0)?.notifications.is_empty());
        super::ingest_remote(&daemon, &session, &notice(15))?;
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"reset","id":14}),
        )?;
        assert_eq!(daemon.snapshot(None, 0)?.notifications.len(), 1);
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"replacement"}),
        )?;
        assert!(daemon.snapshot(None, 0)?.notifications.is_empty());
        super::ingest_remote(&daemon, &session, &notice(11))?;
        assert_eq!(daemon.snapshot(None, 0)?.notifications.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn remote_notification_does_not_complete_an_identified_main_prompt() -> anyhow::Result<()>
    {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (client, server) = tokio::io::duplex(32 * 1024);
            let (finish, wait_finish) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                let _auth = socket.next().await.unwrap()?;
                for frame in [
                    serde_json::json!({"type":"hello","protocol_version":1,"session_id":7,"controller":true,
                        "command_completion":true,"ownership_fencing":true,"prompt_completion":true,
                        "command_request_completion":true,"instance_id":"notification-test"}),
                    serde_json::json!({"type":"snapshot","scrollback":[{"type":"notify","id":1,
                        "title":"Old completion","body":"Already seen","mode":"always"}]}),
                ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                let prompt: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(prompt["type"], "submit_prompt");
                for _ in 0..2 {
                    socket.send(Message::Text(serde_json::json!({"type":"notify","id":2,
                        "title":"Remote finished","body":"A separate completion","mode":"always"}).to_string().into())).await?;
                }
                wait_finish.await?;
                socket.send(Message::Text(serde_json::json!({"type":"prompt_done", "request_id":prompt["request_id"]}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"type":"bye","reason":"fixture complete"}).to_string().into())).await?;
                Ok::<_, anyhow::Error>(())
            });
            let (_tx, mut rx) = tokio::sync::mpsc::channel(4);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let worker = tokio::spawn({ let daemon = std::sync::Arc::clone(&daemon); let session = session.clone();
                async move { super::remote_socket(daemon, &session, socket, "secret", Some("Main task"), &mut rx).await } });
            loop {
                if !daemon.snapshot(None, 0)?.notifications.is_empty() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            assert_eq!(snapshot.notifications.len(), 1);
            assert_eq!(snapshot.notifications[0].title, "Remote finished");
            assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Running);
            assert!(!snapshot.entries.iter().any(|entry| entry.text.contains("Remote finished") || entry.text.contains("Old completion")));
            finish.send(()).unwrap();
            worker.await??;
            peer.await??;
            assert_ne!(daemon.snapshot(None, 0)?.sessions[0].state, crate::model::RunState::Running);
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[tokio::test]
    async fn notification_feed_bounds_and_retires_invalid_session_generations() -> anyhow::Result<()>
    {
        let (_daemon, session, _directory) = remote_restart_fixture()?;
        let now = 100_000;
        let mut feed = crate::notifications::Feed::default();
        for _ in 0..70 {
            feed.push(
                crate::notifications::Notification::from_remote(
                    &session.id,
                    session.generation,
                    &serde_json::json!({"title":"Done","body":"","mode":"always"}),
                    now,
                )
                .unwrap(),
                now,
            );
        }
        assert_eq!(feed.snapshot(std::slice::from_ref(&session), now).len(), 64);
        let mut retired = session.clone();
        retired.generation += 1;
        assert!(feed.snapshot(&[retired], now).is_empty());
        assert!(feed.snapshot(&[session], now).is_empty());
        Ok(())
    }

    #[tokio::test]
    #[cfg(feature = "gui")]
    async fn remote_buddy_observer_receives_pngs_and_clears_without_prompt_or_replay()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (client, server) = tokio::io::duplex(32 * 1024);
            let (clear, wait_clear) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["type"], "observe_auth");
                assert_eq!(auth["buddy_stream"], true);
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"hello","protocol_version":1,"session_id":7,
                    "controller":false,"ownership_fencing":true,"instance_id":"buddy-test"}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"snapshot","scrollback":[],"highest_id":null}).to_string().into())).await?;
                let rgba = image::RgbaImage::from_pixel(2, 2, image::Rgba([12, 34, 56, 128]));
                let mut encoded = std::io::Cursor::new(Vec::new());
                rgba.write_to(&mut encoded, image::ImageFormat::Png)?;
                let frame = crate::buddyframe::Frame { revision:1, png:encoded.into_inner() };
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"buddy","snapshot":frame}).to_string().into())).await?;
                wait_clear.await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"buddy","snapshot":null}).to_string().into())).await?;
                assert!(matches!(socket.next().await.unwrap()?, Message::Close(_)));
                Ok::<_, anyhow::Error>(())
            });
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let worker = tokio::spawn({ let daemon = std::sync::Arc::clone(&daemon); let session = session.clone();
                async move { super::remote_socket(daemon, &session, socket, "secret", None, &mut rx).await } });
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry.get(&session.id).is_some_and(|value| value.buddy.is_some()) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            let frame = snapshot.telemetry[&session.id].buddy.as_ref().unwrap();
            assert!(frame.is_valid());
            assert_eq!(frame.dimensions(), Some((2, 2)));
            assert!(!snapshot.sessions[0].attention);
            assert!(snapshot.entries.is_empty());
            assert!(!snapshot.telemetry[&session.id].remote_access.as_ref().unwrap().controller);
            clear.send(()).unwrap();
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].buddy.is_none() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::Stop).await?;
            worker.await??;
            peer.await??;
            assert!(daemon.snapshot(Some(&session.id), 0)?.entries.is_empty());
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[tokio::test]
    async fn job_output_cancel_remote_preserves_busy_turn_and_fences_observers()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (client, server) = tokio::io::duplex(32 * 1024);
            let (finish, wait_finish) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                let _auth = socket.next().await.unwrap()?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"hello","protocol_version":1,"session_id":7,
                    "controller":true,"command_completion":true,"ownership_fencing":true,"prompt_completion":true,
                    "command_request_completion":true,"instance_id":"job-output-test"}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"snapshot","scrollback":[],"highest_id":null}).to_string().into())).await?;
                let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(command["type"], "submit_command");
                // Role handoff is permitted while a graphical interaction is active.
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"frame","frame_id":"output-frame",
                    "snapshot":{"id":"output-frame","component":"demo","w":1,"h":1,
                        "cells":[{"x":0,"y":0,"ch":"X","fg":[1,2,3],"bg":null,"bold":false}]}
                }).to_string().into())).await?;
                let mut metrics = serde_json::json!({"v":1,"type":"metrics","snapshot":{"kind":"job_output","snapshot":{
                    "id":1,"page":1,"total":4,"start":0,"end":4,"text":"live","lossy":false,
                    "tail":true,"following":true,"observer":"remote-observer"}}});
                socket.send(Message::Text(metrics.to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"control","controller":false}).to_string().into())).await?;
                let claim: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(claim["type"], "request_control", "observer stop must never reach host");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"control","controller":true}).to_string().into())).await?;
                let stop: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(stop["type"], "job_output_stop");
                assert_eq!(stop["observer"], "remote-observer", "stale stop must never reach host");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"job_output_rejected","observer":"remote-observer","reason":"fixture retry"}).to_string().into())).await?;
                let retry: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(retry, stop);
                metrics["snapshot"]["snapshot"]["following"] = false.into();
                socket.send(Message::Text(metrics.to_string().into())).await?;
                wait_finish.await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"command_request_done","request_id":command["request_id"]}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"bye","reason":"fixture complete"}).to_string().into())).await?;
                Ok::<_, anyhow::Error>(())
            });
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let worker = tokio::spawn({ let daemon = std::sync::Arc::clone(&daemon); let session = session.clone();
                async move { super::remote_socket(daemon, &session, socket, "secret", Some("/jobs follow 1"), &mut rx).await } });
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry.get(&session.id).is_some_and(|value|
                    value.metrics.contains_key("job_output") && value.remote_access.as_ref().is_some_and(|access| !access.controller)) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::StopJobOutput("remote-observer".into())).await?;
            tx.send(super::Input::RemoteControl(true)).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].remote_access.as_ref().is_some_and(|access| access.controller) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::StopJobOutput("stale-observer".into())).await?;
            tx.send(super::Input::StopJobOutput("remote-observer".into())).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.entries.iter().any(|entry| entry.text.contains("fixture retry")) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(daemon.store.lock().unwrap().session(&session.id)?.state, crate::model::RunState::Running);
            tx.send(super::Input::StopJobOutput("remote-observer".into())).await?;
            loop {
                if matches!(&daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["job_output"], crate::model::EngineMetrics::JobOutput {snapshot} if !snapshot.following) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(daemon.store.lock().unwrap().session(&session.id)?.state, crate::model::RunState::Running);
            finish.send(()).unwrap();
            worker.await??;
            peer.await??;
            assert!(!daemon.snapshot(Some(&session.id), 0)?.entries.iter().any(|entry| entry.role == "user"));
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[tokio::test]
    async fn remote_live_settings_keep_the_active_request_and_report_rejections()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (client, server) = tokio::io::duplex(32 * 1024);
            let (finish, wait_finish) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                let _auth = socket.next().await.unwrap()?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"hello","protocol_version":1,"session_id":7,
                    "controller":true,"command_completion":true,"ownership_fencing":true,"prompt_completion":true,
                    "command_request_completion":true,"instance_id":"live-control-test"}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"snapshot","scrollback":[],"highest_id":null}).to_string().into())).await?;
                let main: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(main["type"], "submit_prompt");
                let mut controls = serde_json::json!({"v":1,"type":"controls","controls":{"version":1,"commands":[],
                    "features":[{"name":"remote_live_controls","label":"Live settings","available":true,"detail":"Independent settings"}],
                    "settings":{"model":"remote-fixture","temperature":0.5,"power_percent":100,"speculative":false,
                        "speculation_available":false,"micro_compaction":true,"loop_guard":true,"steering":null}}});
                socket.send(Message::Text(controls.to_string().into())).await?;
                let control: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(control["type"], "live_control");
                assert_eq!(control["request"], serde_json::json!({"control":"micro_compaction","enabled":false}));
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"live_control_rejected","request":control["request"],"reason":"fixture retry"}).to_string().into())).await?;
                let retry: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(retry, control);
                controls["controls"]["settings"]["micro_compaction"] = false.into();
                controls["controls"]["features"].as_array_mut().unwrap().push(serde_json::json!({"name":"live_hints","label":"Hints","available":true,"detail":"Mid-turn hints"}));
                socket.send(Message::Text(controls.to_string().into())).await?;
                let hints: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(hints["type"], "live_control");
                assert_eq!(hints["request"], serde_json::json!({"control":"hints","enabled":true}));
                controls["controls"]["settings"]["hints"] = true.into();
                socket.send(Message::Text(controls.to_string().into())).await?;
                wait_finish.await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"prompt_done","request_id":main["request_id"]}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"bye","reason":"fixture complete"}).to_string().into())).await?;
                Ok::<_, anyhow::Error>(())
            });
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let worker = tokio::spawn({ let daemon = std::sync::Arc::clone(&daemon); let session = session.clone();
                async move { super::remote_socket(daemon, &session, socket, "secret", Some("Main task"), &mut rx).await } });
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.sessions[0].can_receive_live_controls() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::LiveControl(crate::livecontrol::LiveControl::Hints(true))).await?;
            tx.send(super::Input::LiveControl(crate::livecontrol::LiveControl::MicroCompaction(false))).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.entries.iter().any(|entry| entry.text.contains("Live setting rejected: fixture retry")) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(daemon.store.lock().unwrap().session(&session.id)?.state, crate::model::RunState::Running);
            tx.send(super::Input::LiveControl(crate::livecontrol::LiveControl::MicroCompaction(false))).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.sessions[0].controls.as_ref().and_then(|controls| controls.settings.as_ref()).is_some_and(|settings| !settings.micro_compaction) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert_eq!(daemon.store.lock().unwrap().session(&session.id)?.state, crate::model::RunState::Running);
            tx.send(super::Input::LiveControl(crate::livecontrol::LiveControl::Hints(true))).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.sessions[0].controls.as_ref().and_then(|controls| controls.settings.as_ref()).is_some_and(|settings| settings.hints == Some(true)) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            finish.send(()).unwrap();
            worker.await??;
            peer.await??;
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[tokio::test]
    async fn remote_frames_keep_input_separate_from_main_command_completion() -> anyhow::Result<()>
    {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (client, server) = tokio::io::duplex(32 * 1024);
            let (notified, wait) = tokio::sync::oneshot::channel();
            let peer = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["frame_stream"], true);
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"hello","protocol_version":1,"session_id":7,
                    "controller":true,"command_completion":true,"ownership_fencing":true,"prompt_completion":true,
                    "command_request_completion":true,"instance_id":"frames-test"}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"snapshot","scrollback":[],"highest_id":null}).to_string().into())).await?;
                let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(command["type"], "submit_command");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"frame","frame_id":"remote-frame",
                    "snapshot":{"id":"remote-frame","component":"demo","w":2,"h":1,
                        "cells":[{"x":0,"y":0,"ch":"X","fg":[1,2,3],"bg":null,"bold":false}]}
                }).to_string().into())).await?;
                let release: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(release["type"], "release_control");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"control","controller":false}).to_string().into())).await?;
                let claim: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(claim["type"], "request_control");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"control","controller":true}).to_string().into())).await?;
                let key: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(key["type"], "frame_input");
                assert_eq!(key["request"]["id"], "remote-frame");
                assert_eq!(key["request"]["text"], "A");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"frame_rejected",
                    "frame_id":"remote-frame","reason":"fixture rejection"}).to_string().into())).await?;
                notified.send(()).unwrap();
                let close: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(close["type"], "frame_input");
                assert_eq!(close["request"]["event"], "close");
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"frame","frame_id":"remote-frame","snapshot":null}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"command_request_done","request_id":command["request_id"]}).to_string().into())).await?;
                socket.send(Message::Text(serde_json::json!({"v":1,"type":"bye","reason":"fixture complete"}).to_string().into())).await?;
                Ok::<_, anyhow::Error>(())
            });
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let worker = tokio::spawn({ let daemon = std::sync::Arc::clone(&daemon); let session = session.clone();
                async move { super::remote_socket(daemon, &session, socket, "secret", Some("/frame demo"), &mut rx).await } });
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry.get(&session.id).is_some_and(|value| value.frame.is_some()) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::RemoteControl(false)).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].remote_access.as_ref().is_some_and(|access| !access.controller) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::FrameControl(crate::frame::Request { id:"remote-frame".into(), input:crate::frame::Input::Key { code:"observer".into(), text:None } })).await?;
            tx.send(super::Input::RemoteControl(true)).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].remote_access.as_ref().is_some_and(|access| access.controller) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            tx.send(super::Input::FrameControl(crate::frame::Request { id:"old-frame".into(), input:crate::frame::Input::Close })).await?;
            tx.send(super::Input::FrameControl(crate::frame::Request { id:"remote-frame".into(), input:crate::frame::Input::Key { code:"a".into(), text:Some('A') } })).await?;
            wait.await?;
            assert_eq!(daemon.store.lock().unwrap().session(&session.id)?.state, crate::model::RunState::Running);
            tx.send(super::Input::FrameControl(crate::frame::Request { id:"remote-frame".into(), input:crate::frame::Input::Close })).await?;
            worker.await??;
            peer.await??;
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            assert!(snapshot.telemetry.get(&session.id).is_none_or(|value| value.frame.is_none()));
            assert!(!snapshot.entries.iter().any(|entry| entry.text.contains("frame_input")));
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[tokio::test]
    async fn remote_restart_reconnects_without_replay_and_requires_fresh_control_for_startup_choices()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let (daemon, session, _directory) = remote_restart_fixture()?;
            let (old_client, old_server) = tokio::io::duplex(32 * 1024);
            let (new_client, new_server) = tokio::io::duplex(32 * 1024);
            let old = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(old_server, Role::Server, None).await;
                let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["type"], "auth");
                for frame in [
                    serde_json::json!({"type":"hello","protocol_version":1,"controller":true,"session_id":7,"instance_id":"old-instance","command_completion":true,"prompt_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                    serde_json::json!({"type":"snapshot","scrollback":[{"type":"visible","id":1,"text":"Old engine"}]}),
                ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(command["text"], "/engines");
                for frame in [
                    serde_json::json!({"type":"command_request_done","request_id":command["request_id"]}),
                    serde_json::json!({"type":"restarting","instance_id":"old-instance"}),
                    serde_json::json!({"type":"bye","reason":"server shutting down"}),
                ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                let _ = socket.next().await;
                Ok::<_, anyhow::Error>(())
            });
            let new = tokio::spawn(async move {
                let mut socket = WebSocketStream::from_raw_socket(new_server, Role::Server, None).await;
                let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["type"], "observe_auth");
                assert!(auth.get("resume_from").is_none());
                for frame in [
                    serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"session_id":7,"instance_id":"new-instance","command_completion":true,"prompt_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                    serde_json::json!({"type":"snapshot","scrollback":[{"type":"visible","id":1,"text":"New engine"}]}),
                    serde_json::json!({"type":"question","request":{"id":"engine-choice","header":"Engine","question":"Choose an engine","options":[{"label":"Use model","description":"Load this model"}],"multi":false}}),
                ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                let request: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(request["type"], "request_control", "the old command must never be replayed");
                socket.send(Message::Text(serde_json::json!({"type":"control","controller":true}).to_string().into())).await?;
                let answer: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(answer["type"], "answer");
                assert_eq!(answer["question_id"], "engine-choice");
                socket.send(Message::Text(serde_json::json!({"type":"question_done","question_id":"engine-choice"}).to_string().into())).await?;
                let prompt: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(prompt["text"], "Fresh input");
                for frame in [
                    serde_json::json!({"type":"visible","id":2,"text":"Fresh answer"}),
                    serde_json::json!({"type":"prompt_done","request_id":prompt["request_id"]}),
                    serde_json::json!({"type":"bye","reason":"fixture complete"}),
                ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                let _ = socket.next().await;
                Ok::<_, anyhow::Error>(())
            });
            let mut sockets = std::collections::VecDeque::from([
                WebSocketStream::from_raw_socket(old_client, Role::Client, None).await,
                WebSocketStream::from_raw_socket(new_client, Role::Client, None).await,
            ]);
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let worker_daemon = std::sync::Arc::clone(&daemon);
            let worker_session = session.clone();
            let worker = tokio::spawn(async move {
                super::remote_with_connector(worker_daemon, &worker_session, "fixture", Some("/engines"), &mut rx,
                    || std::future::ready(sockets.pop_front().ok_or_else(|| anyhow::anyhow!("unexpected reconnect")))).await
            });
            let approval = loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if let Some(approval) = snapshot.sessions[0].approvals.first() {
                    let access = snapshot.telemetry[&session.id].remote_access.as_ref().unwrap();
                    assert!(!access.controller && !access.requesting && !access.waiting_input);
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    break approval.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            };
            tx.send(super::Input::RemoteControl(true)).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.telemetry.get(&session.id)
                    .and_then(|telemetry| telemetry.remote_access.as_ref()).is_some_and(|access| access.controller) { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Approval { approval, choice: "1".into() }).await?;
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.sessions[0].approvals.is_empty() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("Fresh input".into())).await?;
            worker.await??;
            old.await??;
            new.await??;
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            assert_eq!(snapshot.sessions[0].generation, session.generation);
            assert!(!snapshot.sessions[0].recovery_required);
            let output = snapshot.entries.iter().filter(|entry| entry.role == "assistant").map(|entry| entry.text.as_str()).collect::<Vec<_>>();
            assert_eq!(output, ["Old engine", "New engine", "Fresh answer"]);
            assert_eq!(snapshot.entries.iter().filter(|entry| entry.role == "system").count(), 1);
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_stop_bye_is_clean_only_after_matching_completion() -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for (submit, complete) in [(true, true), (true, false), (false, false)] {
                let directory = tempfile::tempdir()?;
                let daemon = crate::daemon::Daemon::open(
                    &directory.path().join("state.sqlite"), "secret".into(),
                )?;
                let session = {
                    let store = daemon.store.lock().unwrap();
                    let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
                    let id = store.create_session(&workspace, "Agent", crate::model::Backend::Remote {
                        url: "ws://127.0.0.1:9100".into(), token_env: "UNUSED_FIXTURE_TOKEN".into(),
                    })?;
                    store.session(&id)?
                };
                let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
                let server = tokio::spawn(async move {
                    let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                        server_stream, tokio_tungstenite::tungstenite::protocol::Role::Server, None,
                    ).await;
                    let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(auth["type"], if submit { "auth" } else { "observe_auth" });
                    for frame in [
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":submit,"session_id":7,"instance_id":"remote-stop-fixture","command_completion":true,"prompt_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                        serde_json::json!({"type":"snapshot","scrollback":[]}),
                    ] {
                        socket.send(Message::Text(frame.to_string().into())).await?;
                    }
                    if submit {
                        let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                        assert_eq!(command["text"], "/rc off");
                        if complete {
                            socket.send(Message::Text(serde_json::json!({"type":"command_request_done","request_id":command["request_id"]}).to_string().into())).await?;
                        }
                    }
                    socket.send(Message::Text(serde_json::json!({"type":"bye","reason":"server shutting down"}).to_string().into())).await?;
                    // Keep the transport alive until the client consumes bye.
                    let _ = socket.next().await;
                    Ok::<_, anyhow::Error>(())
                });
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    client_stream, tokio_tungstenite::tungstenite::protocol::Role::Client, None,
                ).await;
                let result = super::remote_socket(std::sync::Arc::clone(&daemon), &session, socket, "fixture", submit.then_some("/rc off"), &mut rx).await;
                assert!(tx.is_closed(), "bye must fence submissions before the worker exits");
                server.await??;
                if submit && !complete {
                    assert!(result.unwrap_err().to_string().contains("before completing accepted input"));
                } else {
                    result?;
                    let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    assert!(!snapshot.sessions[0].recovery_required);
                    assert!(!snapshot.telemetry.contains_key(&session.id));
                    assert!(snapshot.entries.iter().any(|entry| entry.role == "notice" && entry.text == "server shutting down"));
                }
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_instance_handshake_refuses_credential_reuse_before_binding_replay()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().unwrap();
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"original"}),
        )?;
        daemon.store.lock().unwrap().append_remote(
            &session.id,
            1,
            "assistant",
            "Original replay",
        )?;
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                server_stream,
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                None,
            )
            .await;
            socket.next().await.unwrap()?;
            socket.send(Message::Text(serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"instance_id":"fixture"}).to_string().into())).await?;
            Ok::<_, anyhow::Error>(())
        });
        let (_tx, mut rx) = tokio::sync::mpsc::channel(1);
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client_stream,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let error = super::remote_socket(
            std::sync::Arc::clone(&daemon),
            &session,
            socket,
            "fixture",
            None,
            &mut rx,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("independent"));
        assert!(!error.to_string().contains("fixture"));
        server.await??;
        let mut store = daemon.store.lock().unwrap();
        assert!(!store.bind_remote_instance(&session.id, session.generation, Some("original"))?);
        store.append_remote(&session.id, 1, "assistant", "Must remain a duplicate")?;
        assert_eq!(store.snapshot(Some(&session.id), 0)?.entries.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn remote_instance_changes_retire_host_dialogs_controls_telemetry_and_exports()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Agent",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"original"}),
        )?;
        {
            let store = daemon.store.lock().expect("store poisoned");
            store.controls(
                &session.id,
                session.generation,
                serde_json::from_value(serde_json::json!({"version":1,"commands":[]}))?,
            )?;
            store.approval(
                &session.id,
                session.generation,
                crate::model::PendingApproval {
                    document: None,
                    id: "old-dialog".into(),
                    requirement: serde_json::json!({"kind":"old"}),
                    tool: "question".into(),
                    arguments: String::new(),
                    choices: vec![],
                },
            )?;
        }
        daemon.telemetry.lock().unwrap().insert(
            session.id.clone(),
            (
                session.generation,
                crate::model::EngineTelemetry {
                    task_report: Some("old counters".into()),
                    ..Default::default()
                },
            ),
        );
        daemon.contexts.lock().unwrap().insert(
            session.id.clone(),
            (
                session.generation,
                crate::model::SessionContext {
                    branch: Some("old branch".into()),
                    ..Default::default()
                },
            ),
        );
        for event in [
            serde_json::json!({"phase":"begin", "id":"old-export", "name":"old.md", "format":"md", "bytes":5, "sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"}),
            serde_json::json!({"phase":"chunk", "id":"old-export", "offset":0, "text":"hello"}),
            serde_json::json!({"phase":"end", "id":"old-export", "bytes":5}),
        ] {
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"artifact","event":event}),
            )?;
        }
        let before = daemon.snapshot(Some(&session.id), 0)?;
        assert!(before.telemetry[&session.id].export.is_some());
        for invalid in [
            serde_json::json!({"instance_id":7}),
            serde_json::json!({"instance_id":"bad identity"}),
        ] {
            assert!(super::bind_remote_instance(&daemon, &session, &invalid).is_err());
        }
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"original"}),
        )?;
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .export
                .is_some()
        );
        super::bind_remote_instance(
            &daemon,
            &session,
            &serde_json::json!({"instance_id":"replacement"}),
        )?;
        let after = daemon.snapshot(Some(&session.id), 0)?;
        assert!(after.sessions[0].controls.is_none() && after.sessions[0].approvals.is_empty());
        assert!(
            !after.telemetry.contains_key(&session.id) && !after.contexts.contains_key(&session.id)
        );
        assert!(
            daemon
                .artifacts
                .lock()
                .unwrap()
                .metadata(&session.id, session.generation)
                .is_empty()
        );
        assert_eq!(after.sessions[0].generation, session.generation);
        assert_eq!(after.entries.len(), before.entries.len() + 1);
        Ok(())
    }

    #[tokio::test]
    async fn remote_instance_handshakes_deduplicate_same_server_replay_and_accept_restarted_sequences()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let directory = tempfile::tempdir()?;
            let daemon = crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
            let session = {
                let store = daemon.store.lock().unwrap();
                let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
                let id = store.create_session(&workspace, "Agent", crate::model::Backend::Remote { url:"ws://127.0.0.1:9100".into(), token_env:"UNUSED_FIXTURE_TOKEN".into() })?;
                store.session(&id)?
            };
            for (index, instance, text) in [(0,"one","First server"),(1,"one","First server"),(2,"two","Restarted server")] {
                let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
                let server = tokio::spawn(async move {
                    let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(server_stream, tokio_tungstenite::tungstenite::protocol::Role::Server, None).await;
                    let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(auth["type"], "observe_auth");
                    for frame in [
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"instance_id":instance}),
                        serde_json::json!({"type":"snapshot","scrollback":[{"type":"visible","id":1,"text":text}]}),
                        serde_json::json!({"type":"tasks","completed":0,"total":0,"report":format!("ready-{index}")}),
                    ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                    assert!(socket.next().await.unwrap()?.is_close(), "Attachment must not submit or replay input");
                    Ok::<_, anyhow::Error>(())
                });
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                let worker_daemon = std::sync::Arc::clone(&daemon);
                let worker_session = session.clone();
                let worker = tokio::spawn(async move {
                    let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(client_stream, tokio_tungstenite::tungstenite::protocol::Role::Client, None).await;
                    super::remote_socket(worker_daemon, &worker_session, socket, "fixture", None, &mut rx).await
                });
                loop {
                    let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                    if snapshot.telemetry.get(&session.id).and_then(|telemetry| telemetry.task_report.as_deref()) == Some(format!("ready-{index}").as_str()) { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                drop(tx);
                worker.await??;
                server.await??;
            }
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            let output = snapshot.entries.iter().filter(|entry| entry.role == "assistant").map(|entry| entry.text.as_str()).collect::<Vec<_>>();
            assert_eq!(output, ["First server", "Restarted server"]);
            assert_eq!(snapshot.entries.iter().filter(|entry| entry.role == "system").count(), 1);
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_asides_work_during_owned_turns_and_without_observer_control()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for controller in [true, false] {
                let directory = tempfile::tempdir()?;
                let daemon = crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
                let session = {
                    let store = daemon.store.lock().expect("store poisoned");
                    let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
                    let id = store.create_session(&workspace, "Agent", crate::model::Backend::Remote {
                        url: "ws://127.0.0.1:9100".into(), token_env: "UNUSED_FIXTURE_TOKEN".into(),
                    })?;
                    store.session(&id)?
                };
                let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
                let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
                let server = tokio::spawn(async move {
                    let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                        server_stream, tokio_tungstenite::tungstenite::protocol::Role::Server, None,
                    ).await;
                    let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(auth["type"], if controller { "auth" } else { "observe_auth" });
                    for value in [
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":controller,"prompt_completion":true,"ownership_fencing":true}),
                        serde_json::json!({"type":"snapshot","scrollback":[]}),
                    ] {
                        socket.send(Message::Text(value.to_string().into())).await?;
                    }
                    let main_id = if controller {
                        let main: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                        assert_eq!(main["type"], "submit_prompt");
                        assert_eq!(main["text"], "Main task");
                        Some(main["request_id"].clone())
                    } else { None };
                    ready_tx.send(()).unwrap();
                    let aside: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(aside["type"], "btw");
                    assert_eq!(aside["text"], "Why 🦀?\nExplain.");
                    for value in [
                        serde_json::json!({"type":"btw_begin"}),
                        serde_json::json!({"type":"aside","id":1,"role":"answer","text":"Side answer"}),
                        serde_json::json!({"type":"btw_end"}),
                        serde_json::json!({"type":"tasks","completed":0,"total":0,"report":"Aside finished"}),
                    ] {
                        socket.send(Message::Text(value.to_string().into())).await?;
                    }
                    finish_rx.await?;
                    if let Some(request_id) = main_id {
                        socket.send(Message::Text(serde_json::json!({"type":"prompt_done","request_id":request_id}).to_string().into())).await?;
                    }
                    let close = socket.next().await.unwrap()?;
                    assert!(close.is_close());
                    Ok::<_, anyhow::Error>(())
                });
                let (tx, mut rx) = tokio::sync::mpsc::channel(1);
                let worker_daemon = std::sync::Arc::clone(&daemon);
                let worker_session = session.clone();
                let worker = tokio::spawn(async move {
                    let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                        client_stream, tokio_tungstenite::tungstenite::protocol::Role::Client, None,
                    ).await;
                    super::remote_socket(worker_daemon, &worker_session, socket, "fixture", controller.then_some("Main task"), &mut rx).await
                });
                ready_rx.await?;
                tx.send(super::Input::Aside("Why 🦀?\nExplain.".into())).await?;
                loop {
                    let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                    if snapshot.telemetry.get(&session.id).and_then(|telemetry| telemetry.task_report.as_deref()) == Some("Aside finished") {
                        assert_eq!(snapshot.asides.len(), 1);
                        assert_eq!(snapshot.asides[0].text, "Side answer");
                        assert!(snapshot.entries.is_empty());
                        assert_eq!(snapshot.sessions[0].state, if controller { crate::model::RunState::Running } else { crate::model::RunState::Idle });
                        assert_eq!(snapshot.telemetry[&session.id].remote_access.as_ref().unwrap().controller, controller);
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                finish_tx.send(()).unwrap();
                if controller {
                    while daemon.snapshot(Some(&session.id), 0)?.sessions[0].state != crate::model::RunState::Idle {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
                drop(tx);
                worker.await??;
                server.await??;
            }
            Ok::<_, anyhow::Error>(())
        }).await??;
        Ok(())
    }

    #[tokio::test]
    async fn aside_frames_keep_distinct_roles_and_reject_replaced_workers() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Asides", directory.path().to_str().unwrap())?;
            let id = store.create_session(&workspace, "Session", crate::model::Backend::Demo)?;
            store.session(&id)?
        };
        for (index, role) in ["answer", "thinking", "tool", "notice", "error"]
            .into_iter()
            .enumerate()
        {
            let frame = serde_json::json!({"type":"aside", "id":index + 1, "role":role, "text":format!("side {role}")});
            super::ingest_remote(&daemon, &session, &frame)?;
            super::ingest_remote(&daemon, &session, &frame)?;
        }
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"visible", "id":6, "text":"main answer"}),
        )?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot
                .asides
                .iter()
                .filter(|entry| entry.role == "aside")
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .filter(|entry| entry.role == "assistant")
                .count(),
            1
        );
        assert!(
            snapshot
                .asides
                .iter()
                .filter(|entry| entry.text.starts_with("side "))
                .all(|entry| entry.role.starts_with("aside"))
        );
        assert_eq!(snapshot.asides.len(), 5);
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| !entry.role.starts_with("aside"))
        );
        let malformed = serde_json::json!({"type":"aside", "role":"unknown", "text":"bad"});
        assert!(super::ingest_remote(&daemon, &session, &malformed).is_err());
        daemon.execute(crate::model::Command::Reset {
            session_id: session.id.clone(),
        })?;
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"aside", "role":"answer", "text":"stale answer"}),
        )?;
        assert!(
            !daemon
                .snapshot(Some(&session.id), 0)?
                .asides
                .iter()
                .any(|entry| entry.text == "stale answer")
        );
        Ok(())
    }

    #[test]
    fn graphical_buddy_updates_are_transient_and_do_not_change_attention_or_old_generations()
    -> anyhow::Result<()> {
        let (daemon, session, _directory) = remote_restart_fixture()?;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(2_u32.to_be_bytes());
        png.extend(2_u32.to_be_bytes());
        let frame = crate::buddyframe::Frame { revision: 1, png };
        let update = serde_json::json!({"type":"buddy", "snapshot":frame});
        super::ingest_telemetry(&daemon, &session, &update)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert!(snapshot.telemetry[&session.id].buddy.is_some());
        assert!(!snapshot.sessions[0].attention);
        assert!(snapshot.entries.is_empty());
        let mut invalid = update.clone();
        invalid["snapshot"]["revision"] = 0.into();
        assert!(super::ingest_telemetry(&daemon, &session, &invalid).is_err());
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .buddy
                .is_some()
        );
        super::ingest_telemetry(
            &daemon,
            &session,
            &serde_json::json!({"type":"buddy", "snapshot":null}),
        )?;
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .buddy
                .is_none()
        );
        daemon.execute(crate::model::Command::Reset {
            session_id: session.id.clone(),
        })?;
        super::ingest_telemetry(&daemon, &session, &update)?;
        assert!(
            daemon
                .snapshot(Some(&session.id), 0)?
                .telemetry
                .get(&session.id)
                .is_none_or(|value| value.buddy.is_none())
        );
        Ok(())
    }

    #[test]
    fn graphical_frame_snapshots_fence_close_identity_attention_and_worker_retirement()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Frames", directory.path().to_str().unwrap())?;
            let id = store.create_session(&workspace, "Frame", crate::model::Backend::Demo)?;
            store.session(&id)?
        };
        let frame = |id: &str| {
            serde_json::json!({"type":"frame", "snapshot": {
                "id":id, "component":"demo", "w":2, "h":1,
                "cells":[{"x":0,"y":0,"ch":"X","fg":[1,2,3],"bg":null,"bold":false}]
            }})
        };
        super::ingest_remote(&daemon, &session, &frame("first"))?;
        assert!(daemon.store.lock().unwrap().session(&session.id)?.attention);
        daemon.store.lock().unwrap().acknowledge(&session.id)?;
        super::ingest_remote(&daemon, &session, &frame("first"))?;
        assert!(!daemon.store.lock().unwrap().session(&session.id)?.attention);
        super::ingest_remote(&daemon, &session, &frame("replacement"))?;
        assert!(daemon.store.lock().unwrap().session(&session.id)?.attention);
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"frame", "id":"first", "snapshot":null}),
        )?;
        assert!(
            super::ingest_remote(&daemon, &session, &serde_json::json!({"type":"frame"})).is_err()
        );
        assert!(
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"frame", "snapshot":null})
            )
            .is_err()
        );
        drop(super::ExportLease {
            daemon: std::sync::Arc::downgrade(&daemon),
            session: session.id.clone(),
            generation: session.generation + 1,
        });
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id].frame.as_ref().unwrap().id,
            "replacement"
        );
        assert!(snapshot.entries.is_empty());
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"frame", "id":"replacement", "snapshot":null}),
        )?;
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .frame
                .is_none()
        );
        super::ingest_remote(&daemon, &session, &frame("third"))?;
        drop(super::ExportLease {
            daemon: std::sync::Arc::downgrade(&daemon),
            session: session.id.clone(),
            generation: session.generation,
        });
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .frame
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn html_preview_snapshots_preserve_independent_artifacts_and_retire_stale_generations()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace =
                store.create_workspace("Preview", directory.path().to_str().unwrap())?;
            let id = store.create_session(&workspace, "Session", crate::model::Backend::Demo)?;
            store.session(&id)?
        };
        for purpose in ["conversation", "insights", "preview"] {
            for event in [
                serde_json::json!({"phase":"begin", "purpose":purpose, "id":purpose,
                    "name":"report.html", "format":"html", "bytes":5,
                    "sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"}),
                serde_json::json!({"phase":"chunk", "id":purpose, "offset":0, "text":"hello"}),
                serde_json::json!({"phase":"end", "id":purpose, "bytes":5}),
            ] {
                super::ingest_remote(
                    &daemon,
                    &session,
                    &serde_json::json!({"type":"artifact", "event":event}),
                )?;
            }
        }
        let original = daemon.snapshot(Some(&session.id), 0)?;
        let counters = &original.telemetry[&session.id];
        assert_eq!(counters.artifacts.len(), 3);
        assert!(counters.export.is_some());
        assert!(counters.insights_export.is_some());
        assert!(counters.preview_export.is_some());
        assert_ne!(
            counters.export.as_ref().unwrap().id,
            counters.preview_export.as_ref().unwrap().id
        );
        daemon.telemetry.lock().expect("telemetry poisoned").clear();
        let reattached = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            reattached.telemetry[&session.id].artifacts,
            counters.artifacts
        );
        assert_eq!(
            reattached.telemetry[&session.id].preview_export,
            counters.preview_export
        );
        assert_eq!(reattached.telemetry[&session.id].export, counters.export);
        assert_eq!(
            reattached.telemetry[&session.id].insights_export,
            counters.insights_export
        );
        assert!(
            !reattached
                .entries
                .iter()
                .any(|entry| entry.text.contains("hello"))
        );
        super::ingest_remote(&daemon, &session, &serde_json::json!({"type":"reset"}))?;
        assert!(
            daemon
                .snapshot(Some(&session.id), 0)?
                .telemetry
                .get(&session.id)
                .is_none_or(|value| value.preview_export.is_none())
        );
        let mut stale = session.clone();
        stale.generation += 1;
        super::ingest_remote(
            &daemon,
            &stale,
            &serde_json::json!({"type":"artifact", "event":{
                "phase":"begin", "id":"stale", "purpose":"preview", "name":"report.html", "format":"html", "bytes":5,
                "sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
            }}),
        )?;
        assert!(
            daemon
                .artifacts
                .lock()
                .expect("artifacts poisoned")
                .metadata(&session.id, stale.generation)
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn export_staging_downloads_require_authentication_and_current_generation()
    -> anyhow::Result<()> {
        use axum::{
            body::Body,
            http::{Request, StatusCode},
        };
        use std::sync::Arc;
        use tower::ServiceExt;
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace =
                store.create_workspace("Exports", directory.path().to_str().unwrap())?;
            let id = store.create_session(&workspace, "Session", crate::model::Backend::Demo)?;
            store.session(&id)?
        };
        let begin = serde_json::json!({"type":"artifact", "event":{
            "phase":"begin", "id":"export-1", "name":"quoted\"name 🦀.html", "format":"html", "bytes":5,
            "sha256":"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        }});
        super::ingest_remote(&daemon, &session, &begin)?;
        assert!(
            daemon
                .snapshot(Some(&session.id), 0)?
                .telemetry
                .get(&session.id)
                .is_none_or(|telemetry| telemetry.export.is_none())
        );
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"artifact", "event":{
                "phase":"chunk", "id":"export-1", "offset":0, "text":"hello"
            }}),
        )?;
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"artifact", "event":{
                "phase":"end", "id":"export-1", "bytes":5
            }}),
        )?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        let export = snapshot.telemetry[&session.id].export.as_ref().unwrap();
        assert_eq!(export.bytes, 5);
        daemon
            .telemetry
            .lock()
            .expect("telemetry poisoned")
            .remove(&session.id);
        let reattached = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            reattached.telemetry[&session.id].export.as_ref().unwrap(),
            export
        );
        assert!(reattached.telemetry[&session.id].status.is_none());
        assert!(
            snapshot
                .entries
                .iter()
                .all(|entry| !entry.text.contains("hello"))
        );
        let router = crate::daemon::router(Arc::clone(&daemon));
        let uri = format!(
            "/v1/export?session={}&generation={}&id={}",
            session.id, session.generation, export.id
        );
        for (authorization, status) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some("Bearer wrong"), StatusCode::UNAUTHORIZED),
            (Some("Bearer secret"), StatusCode::OK),
        ] {
            let mut request = Request::builder().uri(&uri);
            if let Some(authorization) = authorization {
                request = request.header("authorization", authorization);
            }
            let response = router.clone().oneshot(request.body(Body::empty())?).await?;
            assert_eq!(response.status(), status);
            if status == StatusCode::OK {
                assert_eq!(
                    response.headers()["content-type"],
                    "application/octet-stream"
                );
                assert_eq!(response.headers()["cache-control"], "no-store");
                assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                let disposition = response.headers()["content-disposition"].to_str()?;
                assert!(disposition.starts_with("attachment;"));
                assert!(disposition.contains("%22"));
                assert_eq!(
                    axum::body::to_bytes(response.into_body(), 32)
                        .await?
                        .as_ref(),
                    b"hello"
                );
            }
        }
        for uri in [
            format!(
                "/v1/export?session={}&generation={}&id=missing",
                session.id, session.generation
            ),
            format!(
                "/v1/export?session={}&generation={}&id={}",
                session.id,
                session.generation + 1,
                export.id
            ),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("authorization", "Bearer secret")
                        .body(Body::empty())?,
                )
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        // A disconnected worker cannot leave a pending export or affect a new owner.
        super::ingest_remote(&daemon, &session, &begin)?;
        let retirement = serde_json::json!({"type":"artifact", "event":{
            "phase":"abort", "id":"export-1", "reason":"reader stalled"
        }});
        super::ingest_remote(&daemon, &session, &retirement)?;
        super::ingest_remote(&daemon, &session, &retirement)?;
        let retired = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            retired
                .entries
                .iter()
                .filter(|entry| entry.text.starts_with("Export transfer incomplete:"))
                .count(),
            1
        );
        assert_eq!(
            retired.telemetry[&session.id].export.as_ref().unwrap().id,
            export.id
        );
        super::ingest_remote(&daemon, &session, &begin)?;
        drop(super::ExportLease {
            daemon: Arc::downgrade(&daemon),
            session: session.id.clone(),
            generation: session.generation,
        });
        assert!(
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"artifact", "event":{
                    "phase":"end", "id":"export-1", "bytes":0
                }})
            )
            .is_err()
        );
        daemon.execute(crate::model::Command::Reset {
            session_id: session.id.clone(),
        })?;
        super::ingest_remote(&daemon, &session, &begin)?;
        let response = router
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            daemon
                .artifacts
                .lock()
                .expect("artifacts poisoned")
                .file(&session.id, session.generation, &export.id)
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn waiting_remote_input_is_cancelled_without_interrupt_or_late_submission()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        for late_grant in [false, true] {
            let directory = tempfile::tempdir()?;
            let daemon = crate::daemon::Daemon::open(
                &directory.path().join("state.sqlite"),
                "secret".into(),
            )?;
            let session = {
                let store = daemon.store.lock().expect("store poisoned");
                let workspace =
                    store.create_workspace("Waiting", directory.path().to_str().unwrap())?;
                let id = store.create_session(
                    &workspace,
                    "Remote",
                    crate::model::Backend::Remote {
                        url: "ws://127.0.0.1:9100".into(),
                        token_env: "UNUSED_FIXTURE_TOKEN".into(),
                    },
                )?;
                store.session(&id)?
            };
            let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
            let (grant, granted) = tokio::sync::oneshot::channel();
            let server = tokio::spawn(async move {
                let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    server_stream,
                    tokio_tungstenite::tungstenite::protocol::Role::Server,
                    None,
                )
                .await;
                let auth: serde_json::Value =
                    serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(auth["type"], "auth");
                for value in [
                    serde_json::json!({"type":"hello","protocol_version":1,"controller":false,"session_id":7,"command_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                    serde_json::json!({"type":"snapshot","scrollback":[]}),
                ] {
                    socket.send(Message::Text(value.to_string().into())).await?;
                }
                let request: serde_json::Value =
                    serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                assert_eq!(request["type"], "request_control");
                socket.send(Message::Text(serde_json::json!({"type":"control_denied","reason":"awaiting local /grant"}).to_string().into())).await?;
                if late_grant {
                    granted.await?;
                    socket
                        .send(Message::Text(
                            serde_json::json!({"type":"control","controller":true})
                                .to_string()
                                .into(),
                        ))
                        .await?;
                    let release: serde_json::Value =
                        serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(
                        release["type"], "release_control",
                        "stale input must never reach the host"
                    );
                }
                assert!(
                    socket.next().await.unwrap()?.is_close(),
                    "waiting cancellation must not interrupt the host or send input"
                );
                Ok::<_, anyhow::Error>(())
            });
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let worker_daemon = std::sync::Arc::clone(&daemon);
            let worker_session = session.clone();
            let worker = tokio::spawn(async move {
                let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                    client_stream,
                    tokio_tungstenite::tungstenite::protocol::Role::Client,
                    None,
                )
                .await;
                super::remote_socket(
                    worker_daemon,
                    &worker_session,
                    socket,
                    "fixture",
                    Some("/command"),
                    &mut rx,
                )
                .await
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                    if snapshot
                        .telemetry
                        .get(&session.id)
                        .and_then(|telemetry| telemetry.remote_access.as_ref())
                        .is_some_and(|access| access.waiting_input && access.reason.is_some())
                    {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                if late_grant {
                    daemon.store.lock().expect("store poisoned").invalidate(
                        &session.id,
                        crate::model::RunState::Interrupted,
                        false,
                    )?;
                    grant.send(()).unwrap();
                } else {
                    tx.send(super::Input::Stop).await?;
                }
                worker.await??;
                server.await??;
                Ok::<_, anyhow::Error>(())
            })
            .await??;
        }
        Ok(())
    }

    #[tokio::test]
    async fn observer_waits_for_grant_and_releases_without_replaying_commands() -> anyhow::Result<()>
    {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace =
                store.create_workspace("Observer", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Mirror",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let (grant, granted) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel();
        let (command_finish, command_finished) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                server_stream,
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                None,
            )
            .await;
            let auth: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(auth["type"], "observe_auth");
            for value in [
                serde_json::json!({"type":"hello", "protocol_version":1,"controller":false,"session_id":7,"command_completion":true,"prompt_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                serde_json::json!({"type":"snapshot", "scrollback":[]}),
                serde_json::json!({"type":"visible", "id":900,"text":"Observed output"}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let request: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(request["type"], "request_control");
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"control_denied","reason":"awaiting local /grant"})
                        .to_string()
                        .into(),
                ))
                .await?;
            tokio::select! {
                message = socket.next() => anyhow::bail!("Input sent before grant: {message:?}"),
                result = granted => result?,
            }
            for _ in 0..2 {
                socket
                    .send(Message::Text(
                        serde_json::json!({"type":"control","controller":true})
                            .to_string()
                            .into(),
                    ))
                    .await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["type"], "submit_command");
            assert_eq!(command["text"], "/command");
            for value in [
                serde_json::json!({"type":"command_done","text":"/command"}),
                serde_json::json!({"type":"command_request_done","request_id":"another-controller"}),
                serde_json::json!({"type":"tasks","completed":0,"total":0,"report":"Foreign command completion received"}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            tokio::select! {
                message = socket.next() => anyhow::bail!("Command finished or resent before matching ID: {message:?}"),
                result = command_finished => result?,
            }
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]})
                        .to_string()
                        .into(),
                ))
                .await?;
            let release: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(
                release["type"], "release_control",
                "queued input must only be sent once"
            );
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"control","controller":false})
                        .to_string()
                        .into(),
                ))
                .await?;
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"visible","id":901,"text":"Still observing"})
                        .to_string()
                        .into(),
                ))
                .await?;
            let request: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(request["type"], "request_control");
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"control","controller":true})
                        .to_string()
                        .into(),
                ))
                .await?;
            let prompt: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(prompt["type"], "submit_prompt");
            assert_eq!(prompt["text"], "Own task");
            let request_id = prompt["request_id"].as_str().unwrap().to_owned();
            for value in [
                serde_json::json!({"type":"notify","title":"Earlier host task finished"}),
                serde_json::json!({"type":"prompt_done","request_id":"another-client"}),
                serde_json::json!({"type":"tasks","completed":0,"total":0,"report":"Foreign completion received"}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            tokio::select! {
                message = socket.next() => anyhow::bail!("Input closed or resent before own completion: {message:?}"),
                result = finished => result?,
            }
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"prompt_done","request_id":request_id})
                        .to_string()
                        .into(),
                ))
                .await?;
            let cancelled: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(cancelled["type"], "submit_prompt");
            assert_eq!(cancelled["text"], "Cancelled task");
            socket.send(Message::Text(serde_json::json!({"type":"tasks","completed":0,"total":0,"report":"Cancel ready"}).to_string().into())).await?;
            let cancel: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(
                cancel["type"], "cancel_prompt",
                "Stop must not interrupt an unrelated host task"
            );
            assert_eq!(cancel["request_id"], cancelled["request_id"]);
            assert!(socket.next().await.unwrap()?.is_close());
            Ok::<_, anyhow::Error>(())
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let worker_daemon = std::sync::Arc::clone(&daemon);
        let worker_session = session.clone();
        let worker = tokio::spawn(async move {
            let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                client_stream,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await;
            super::remote_socket(
                worker_daemon,
                &worker_session,
                socket,
                "fixture",
                None,
                &mut rx,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.remote_access.as_ref())
                    .is_some_and(|access| !access.controller && !access.waiting_input)
                    && snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.text == "Observed output")
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/command".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.remote_access.as_ref())
                    .is_some_and(|access| {
                        access.waiting_input
                            && access.reason.as_deref() == Some("awaiting local /grant")
                    })
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Starting);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            grant.send(()).unwrap();
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.task_report.as_deref()
                            == Some("Foreign command completion received")
                    })
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Running);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            command_finish.send(()).unwrap();
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.remote_access.as_ref())
                    .is_some_and(|access| access.controller && !access.waiting_input)
                    && snapshot.sessions[0].state == crate::model::RunState::Idle
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::RemoteControl(false)).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.remote_access.as_ref())
                    .is_some_and(|access| !access.controller)
                    && snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.text == "Still observing")
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("Own task".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.task_report.as_deref() == Some("Foreign completion received")
                    })
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Running);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            finish.send(()).unwrap();
            loop {
                if daemon.snapshot(Some(&session.id), 0)?.sessions[0].state
                    == crate::model::RunState::Idle
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("Cancelled task".into()))
                .await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.task_report.as_deref() == Some("Cancel ready")
                    })
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Stop).await?;
            drop(tx);
            worker.await??;
            server.await??;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[tokio::test]
    async fn remote_connection_observes_idle_jobs_and_reuses_its_controller_session()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "UNUSED_FIXTURE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        let server = tokio::spawn(async move {
            let mut socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                server_stream,
                tokio_tungstenite::tungstenite::protocol::Role::Server,
                None,
            )
            .await;
            let auth: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(auth["token"], "fixture");
            for value in [
                serde_json::json!({"type":"hello","protocol_version":1,"controller":true,"command_completion":true,"command_request_completion":true,"ownership_fencing":true}),
                serde_json::json!({"type":"snapshot","scrollback":[]}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/tasks");
            for value in [
                serde_json::json!({"type":"command_request_done","request_id":command["request_id"]}),
                serde_json::json!({"type":"jobs","snapshot":{"running":0,"total":0,"jobs":[]}}),
                serde_json::json!({"type":"tasks","completed":1,"total":1,"report":"Completed while idle"}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["type"], "submit_command");
            assert_eq!(command["text"], "/command");
            socket
                .send(Message::Text(
                    serde_json::json!({"type":"command_request_done","request_id":command["request_id"]})
                        .to_string()
                        .into(),
                ))
                .await?;
            socket.send(Message::Text(serde_json::json!({"type":"tasks","completed":0,"total":0,"report":"Second complete"}).to_string().into())).await?;
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/skills off");
            for value in [
                serde_json::json!({"type":"extensions", "snapshot":{"kind":"skills", "report":"review · Review project", "enabled":false}}),
                serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/kvcache --page 1");
            for value in [
                serde_json::json!({"type":"cache", "snapshot":{"page":1,"total":1,"summary":"total 1 MiB", "rows":[{
                    "fingerprint":"cache-full", "entry":"c".repeat(40), "depth":0, "label":"system", "detail":"model", "status":"1 MiB · hits 3", "pinned":false
                }]}}),
                serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]}),
                serde_json::json!({"type":"downloads", "snapshot":{"pending":true,"cancel_requested":null,"state":{
                    "set":"fixture", "version":1, "current":"weights.gguf", "index":1, "of":2,
                    "done_bytes":25, "total_bytes":100, "rate_bps":10, "phase":"downloading", "error":null
                }}}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/model cancel");
            for value in [
                serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]}),
                serde_json::json!({"type":"downloads", "snapshot":{"pending":true,"cancel_requested":null,"state":{
                    "set":"fixture", "version":1, "current":"weights.gguf", "index":1, "of":2,
                    "done_bytes":25, "total_bytes":100, "rate_bps":0, "phase":"cancelled", "partials_kept":true,"error":null
                }}}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/context");
            for value in [
                serde_json::json!({"type":"metrics", "snapshot":{"kind":"context",
                    "model":"fixture", "window":1000,"used":150,"categories":[30,10,80,20,5]}}),
                serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            let command: serde_json::Value =
                serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
            assert_eq!(command["text"], "/rc status");
            for value in [
                serde_json::json!({"type":"host", "snapshot":{
                    "endpoint":"ws://127.0.0.1:9000/", "holder":"local", "controller":null,
                    "automatic":false,"reserved":false,"pending":[],"pending_total":0}}),
                serde_json::json!({"type":"command_request_done", "request_id":command["request_id"]}),
                serde_json::json!({"type":"host", "snapshot":{
                    "endpoint":"ws://127.0.0.1:9000/", "holder":"local", "controller":null,
                    "automatic":false,"reserved":false,"pending":[8],"pending_total":1}}),
            ] {
                socket.send(Message::Text(value.to_string().into())).await?;
            }
            assert!(socket.next().await.unwrap()?.is_close());
            Ok::<_, anyhow::Error>(())
        });
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        let worker_daemon = std::sync::Arc::clone(&daemon);
        let worker_session = session.clone();
        let worker = tokio::spawn(async move {
            let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
                client_stream,
                tokio_tungstenite::tungstenite::protocol::Role::Client,
                None,
            )
            .await;
            super::remote_socket(
                worker_daemon,
                &worker_session,
                socket,
                "fixture",
                Some("/tasks"),
                &mut rx,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.jobs.is_some()
                            && telemetry.task_report.as_deref() == Some("Completed while idle")
                    })
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/command".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.task_report.as_deref() == Some("Second complete")
                    })
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/skills off".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.extensions.get("skills"))
                    .is_some_and(|inventory| inventory.enabled == Some(false))
                    && snapshot.sessions[0].state == crate::model::RunState::Idle
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/kvcache --page 1".into()))
                .await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| {
                        telemetry.cache.is_some() && telemetry.downloads.is_some()
                    })
                    && snapshot.sessions[0].state == crate::model::RunState::Idle
                {
                    assert_eq!(
                        snapshot.telemetry[&session.id].cache.as_ref().unwrap().rows[0]
                            .command("rm"),
                        Some(format!("/kvcache rm --entry {} cache-full", "c".repeat(40)))
                    );
                    assert_eq!(
                        snapshot.telemetry[&session.id].extensions["skills"].enabled,
                        Some(false)
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let snapshot = daemon.snapshot(Some(&session.id), 0)?;
            let download = snapshot.telemetry[&session.id]
                .downloads
                .as_ref()
                .unwrap()
                .state
                .as_ref()
                .unwrap();
            assert_eq!(download.fraction(), Some(0.25));
            assert!(download.can_cancel());
            tx.send(super::Input::Prompt("/model cancel".into()))
                .await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if let Some(download) = snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.downloads.as_ref())
                    .and_then(|downloads| downloads.state.as_ref())
                    .filter(|download| download.phase == "cancelled")
                {
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    assert_eq!(download.partials_kept, Some(true));
                    assert!(!download.can_cancel());
                    assert!(snapshot.telemetry[&session.id].cache.is_some());
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/context".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if snapshot
                    .telemetry
                    .get(&session.id)
                    .is_some_and(|telemetry| telemetry.metrics.contains_key("context"))
                    && snapshot.sessions[0].state == crate::model::RunState::Idle
                {
                    assert!(matches!(
                        snapshot.telemetry[&session.id].metrics["context"],
                        crate::model::EngineMetrics::Context {
                            used: 150,
                            window: 1000,
                            ..
                        }
                    ));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            tx.send(super::Input::Prompt("/rc status".into())).await?;
            loop {
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                if let Some(host) = snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.host.as_ref())
                    .filter(|host| host.pending == [8])
                {
                    assert!(host.controller.is_none());
                    assert_eq!(snapshot.sessions[0].state, crate::model::RunState::Idle);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            // Dropping input closes the connection after its queued input is processed.
            drop(tx);
            worker.await??;
            server.await??;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }

    #[test]
    fn remote_telemetry_merges_counters_and_fences_replaced_workers() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "PLANK_REMOTE_TOKEN".into(),
                },
            )?;
            store.session(&id)?
        };
        let status = serde_json::json!({"type":"status", "status":{
            "state":"generating", "ctx_used":2048, "ctx_size":8192,
            "generated":50, "gen_tps":32.5, "elapsed_secs":2.0, "think":"med"
        }});
        let tasks = serde_json::json!({"type":"tasks", "completed":2, "total":5,
            "report":"goal: Finish the UI (iteration 2/20, active)\n- [3] in_progress: Wire tasks\n"});
        super::ingest_remote(&daemon, &session, &status)?;
        super::ingest_remote(&daemon, &session, &tasks)?;
        let mut detailed = tasks.clone();
        detailed["goal"] = serde_json::json!("Goal: Finish the UI (2/20, active)");
        detailed["rows"] = serde_json::json!([
            {"id":1,"subject":"Wire tasks","status":"in_progress","active_form":"Wiring tasks"},
            {"id":2,"subject":"Check persistence","status":"pending","active_form":null},
            {"id":3,"subject":"Check recovery","status":"pending","active_form":null},
            {"id":4,"subject":"Commands","status":"completed","active_form":null},
            {"id":5,"subject":"Validation","status":"completed","active_form":null}
        ]);
        super::ingest_remote(&daemon, &session, &detailed)?;
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .goal_label
                .as_deref(),
            Some("Goal: Finish the UI (2/20, active)")
        );
        for goal in [
            serde_json::json!("x".repeat(1025)),
            serde_json::json!("bad\ngoal"),
            serde_json::json!(1),
        ] {
            let mut invalid = detailed.clone();
            invalid["goal"] = goal;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .task_rows
                .as_ref()
                .unwrap()[0]
                .id,
            1
        );
        for (field, value) in [
            ("id", serde_json::json!(0)),
            ("id", serde_json::json!(2)),
            ("subject", serde_json::json!("x".repeat(513))),
            ("active_form", serde_json::json!("bad\nlabel")),
            ("status", serde_json::json!("blocked")),
            ("status", serde_json::json!("completed")),
        ] {
            let mut invalid = detailed.clone();
            invalid["rows"][0][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = detailed.clone();
        invalid["rows"] = serde_json::json!([]);
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .task_rows
                .as_ref()
                .unwrap()[0]
                .status,
            "in_progress"
        );
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id].summary(),
            "generating · Context 2048/8192 · 32.5 tok/s · Think med · Tasks 2/5"
        );
        // Telemetry stays out of both transcript storage and durable session JSON.
        assert!(snapshot.entries.is_empty());
        let jobs = serde_json::json!({"type":"jobs", "snapshot":{
            "running":1, "total":2, "jobs":[
                {"id":1,"pid":10,"elapsed_secs":2.5,"state":"running","exit_code":null,"path":"/tmp/one"},
                {"id":2,"pid":11,"elapsed_secs":1.0,"state":"timed_out","exit_code":-1,"path":"/tmp/two"}
            ]
        }});
        super::ingest_remote(&daemon, &session, &jobs)?;
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .summary()
                .ends_with("Jobs 1")
        );
        for (field, value) in [
            ("pid", serde_json::json!(0)),
            ("elapsed_secs", serde_json::json!(-1)),
            ("exit_code", serde_json::json!(0)),
            ("state", serde_json::json!("unknown")),
        ] {
            let mut invalid = jobs.clone();
            invalid["snapshot"]["jobs"][0][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut duplicate = jobs.clone();
        duplicate["snapshot"]["jobs"][1]["id"] = serde_json::json!(1);
        assert!(super::ingest_remote(&daemon, &session, &duplicate).is_err());
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .jobs
                .as_ref()
                .unwrap()
                .total,
            2
        );
        assert_eq!(
            snapshot.telemetry[&session.id].task_report.as_deref(),
            tasks["report"].as_str()
        );
        let mut oversized = tasks.clone();
        oversized["report"] = serde_json::json!("x".repeat(256 * 1024 + 1));
        assert!(super::ingest_remote(&daemon, &session, &oversized).is_err());
        let mut invalid = status.clone();
        invalid["status"]["gen_tps"] = serde_json::json!(-1);
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        assert!(
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"tasks", "completed":6,"total":5})
            )
            .is_err()
        );
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].tasks,
            Some((2, 5))
        );
        let saved = serde_json::json!({"type":"sessions", "snapshot": {
            "page":1, "total":1, "sessions":[{"id":"friendly-agent", "title":"Review",
                "cwd":"/tmp/project", "tag":"v1", "updated":10}]
        }});
        let config = serde_json::json!({"type":"config", "snapshot": {"page":1, "total":1,
            "fields":[{"path":"ui.showThinking", "label":"Thinking", "value":"true", "hint":"true or false", "editable":true}]}});
        let checkpoints = serde_json::json!({"type":"history", "snapshot":{"kind":"checkpoints", "page":1, "total":1,
            "report":"", "rows":[{"key":"before review", "label":"before review", "detail":"10 messages"}]}});
        let branches = serde_json::json!({"type":"history", "snapshot":{"kind":"forks", "page":1, "total":1,
            "report":"session tree", "rows":[{"key":"1", "label":"Point 1", "detail":"Review code"}]}});
        super::ingest_remote(&daemon, &session, &checkpoints)?;
        super::ingest_remote(&daemon, &session, &branches)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id]
                .checkpoints
                .as_ref()
                .unwrap()
                .rows[0]
                .command("checkpoints")
                .as_deref(),
            Some("/rollback before review")
        );
        assert_eq!(
            snapshot.telemetry[&session.id]
                .branches
                .as_ref()
                .unwrap()
                .rows[0]
                .command("forks")
                .as_deref(),
            Some("/fork 1")
        );
        let mut invalid = branches.clone();
        invalid["snapshot"]["rows"][0]["key"] = "2".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        let mut invalid = checkpoints.clone();
        invalid["snapshot"]["rows"][0]["key"] = "before review\n/quit".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        super::ingest_remote(&daemon, &session, &config)?;
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .config
                .as_ref()
                .unwrap()
                .fields[0]
                .command("false")
                .as_deref(),
            Some("/config ui.showThinking false")
        );
        let mut invalid = config.clone();
        invalid["snapshot"]["fields"][0]["path"] = "ui.showThinking\n/quit".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        super::ingest_remote(&daemon, &session, &saved)?;
        assert_eq!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .saved_sessions
                .as_ref()
                .unwrap()
                .sessions[0]
                .id,
            "friendly-agent"
        );
        for (field, value) in [
            ("page", serde_json::json!(0)),
            ("total", serde_json::json!(2)),
        ] {
            let mut invalid = saved.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = saved.clone();
        invalid["snapshot"]["sessions"][0]["id"] = "friendly-agent\n/quit".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        let reset = serde_json::json!({"type":"reset", "id":500});
        let host = serde_json::json!({"type":"host", "snapshot":{
            "endpoint":"ws://127.0.0.1:9000/", "holder":"local", "controller":null,
            "automatic":false, "reserved":false, "pending":[8,9], "pending_total":2
        }});
        super::ingest_remote(&daemon, &session, &host)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id]
                .host
                .as_ref()
                .unwrap()
                .pending,
            vec![8, 9]
        );
        for (field, value) in [
            ("endpoint", serde_json::json!("ws://public.example:9000/")),
            ("endpoint", serde_json::json!("ws://secret@127.0.0.1:9000/")),
            (
                "endpoint",
                serde_json::json!("ws://127.0.0.1:9000/?token=secret"),
            ),
            ("holder", serde_json::json!("off")),
            ("holder", serde_json::json!("remote")),
            ("controller", serde_json::json!(7)),
            ("automatic", serde_json::json!(true)),
            ("pending", serde_json::json!([-1, 8])),
            ("pending", serde_json::json!([8, 8])),
            ("pending_total", serde_json::json!(3)),
        ] {
            let mut invalid = host.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let context = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"context", "model":"fixture", "window":1000, "used":150,
            "categories":[30,10,80,20,5]
        }});
        let usage = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"usage", "model":"fixture", "input":1234, "output":567,
            "cache_read":89, "cache_write":0, "provider_passes":3, "wall_secs":60.5
        }});
        let speed = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"throughput", "generation":[10.0,12.0], "prefill":[100.0]
        }});
        for frame in [&context, &usage, &speed, &context] {
            super::ingest_remote(&daemon, &session, frame)?;
        }
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        let metrics = &snapshot.telemetry[&session.id].metrics;
        assert_eq!(metrics.len(), 3);
        assert!(matches!(
            metrics["context"],
            crate::model::EngineMetrics::Context { used: 150, .. }
        ));
        assert!(matches!(
            metrics["usage"],
            crate::model::EngineMetrics::Usage {
                input: 1234,
                output: 567,
                ..
            }
        ));
        let subagents = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"subagents", "total":2, "running":1, "rows":[
                {"label":"research", "task":"Inspect repository", "running":true, "elapsed_ms":2000, "prefill":100, "generated":20},
                {"label":"review", "task":"Check change", "running":false, "elapsed_ms":1500, "prefill":90, "generated":15}
            ]
        }});
        super::ingest_remote(&daemon, &session, &subagents)?;
        let mut reports = subagents.clone();
        reports["snapshot"]["rows"][0]["id"] = 1.into();
        reports["snapshot"]["rows"][1]["id"] = 2.into();
        reports["snapshot"]["rows"][1]["report"] =
            "Reviewed the change.\n\n```rust\nfn main() {}\n```".into();
        super::ingest_remote(&daemon, &session, &reports)?;
        let mut stream = reports.clone();
        stream["snapshot"]["rows"][0]["output"] = "Rendered child output\n🦀".into();
        stream["snapshot"]["rows"][0]["output_truncated"] = true.into();
        super::ingest_remote(&daemon, &session, &stream)?;
        for text in [
            serde_json::json!("x".repeat(4097)),
            serde_json::json!("bad\u{1b}text"),
            serde_json::Value::Null,
        ] {
            let mut invalid = stream.clone();
            invalid["snapshot"]["rows"][0]["output"] = text;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut live = reports.clone();
        live["snapshot"]["rows"][0]["live"] = serde_json::json!({"prefilling":true, "prefill_done":5, "prefill_total":10, "generated":0});
        super::ingest_remote(&daemon, &session, &live)?;
        for (field, value) in [
            ("prefill_done", serde_json::json!(11)),
            ("generated", serde_json::json!(u64::MAX)),
        ] {
            let mut invalid = live.clone();
            invalid["snapshot"]["rows"][0]["live"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = live.clone();
        invalid["snapshot"]["rows"][0]["running"] = false.into();
        invalid["snapshot"]["running"] = 0.into();
        assert!(
            super::ingest_remote(&daemon, &session, &invalid).is_err(),
            "finished runs cannot retain live pass counters"
        );
        for (field, value) in [
            ("id", serde_json::json!(1)),
            ("report", serde_json::json!("x".repeat(4097))),
            ("report", serde_json::json!("bad\u{1b}report")),
        ] {
            let mut invalid = reports.clone();
            invalid["snapshot"]["rows"][1][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        assert!(matches!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["subagents"],
            crate::model::EngineMetrics::Subagents {
                running: 1,
                total: 2,
                ..
            }
        ));
        let history = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"subagent_history", "snapshot":{
                "runs":[{"id":7,"label":"review","task":"inspect parser","outcome":"report"}],
                "selected":7,"total":1,"page":1,
                "chunks":[{"role":"assistant","text":"日🦀 completed","continued":false}]
            }
        }});
        super::ingest_remote(&daemon, &session, &history)?;
        let mut forged = history.clone();
        forged["snapshot"]["snapshot"]["selected"] = serde_json::json!(8);
        assert!(super::ingest_remote(&daemon, &session, &forged).is_err());
        let job_output = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"job_output", "snapshot":{"id":5,"page":1,"total":7,"start":0,"end":7,"text":"private","lossy":false}
        }});
        super::ingest_remote(&daemon, &session, &job_output)?;
        let previous_state = daemon.store.lock().unwrap().session(&session.id)?.state;
        daemon.store.lock().unwrap().set_state(
            &session.id,
            crate::model::RunState::Running,
            false,
        )?;
        let mut live_output = job_output.clone();
        live_output["snapshot"]["snapshot"]["tail"] = serde_json::json!(true);
        live_output["snapshot"]["snapshot"]["following"] = serde_json::json!(true);
        super::ingest_remote(&daemon, &session, &live_output)?;
        assert_eq!(
            daemon.store.lock().unwrap().session(&session.id)?.state,
            crate::model::RunState::Running
        );
        daemon
            .store
            .lock()
            .unwrap()
            .set_state(&session.id, previous_state, false)?;
        let mut forged_output = job_output.clone();
        forged_output["snapshot"]["snapshot"]["end"] = serde_json::json!(8);
        assert!(super::ingest_remote(&daemon, &session, &forged_output).is_err());
        assert!(
            matches!(&daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["job_output"],
            crate::model::EngineMetrics::JobOutput { snapshot } if snapshot.id == 5 && snapshot.text == "private")
        );
        assert!(
            matches!(&daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["subagent_history"],
            crate::model::EngineMetrics::SubagentHistory { snapshot } if snapshot.selected == Some(7) && snapshot.chunks[0].text == "日🦀 completed")
        );
        for (field, value) in [
            ("running", serde_json::json!(2)),
            ("total", serde_json::json!(3)),
        ] {
            let mut invalid = subagents.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = subagents.clone();
        invalid["snapshot"]["rows"][0]["label"] = "bad\nlabel".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        for (frame, field, value) in [
            (&context, "used", serde_json::json!(-1)),
            (&context, "window", serde_json::json!(u64::MAX)),
            (&context, "model", serde_json::json!("bad\nmodel")),
            (&context, "categories", serde_json::json!([1, 2, 3])),
            (&usage, "wall_secs", serde_json::json!(-0.1)),
            (&speed, "generation", serde_json::json!(vec![1.0; 65])),
            (&speed, "prefill", serde_json::json!([-1.0])),
            (&speed, "kind", serde_json::json!("unknown")),
        ] {
            let mut invalid = frame.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut days = Vec::new();
        for (month, length) in [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
            .iter()
            .enumerate()
        {
            for day in 1..=*length {
                days.push(serde_json::json!({"date":format!("2025-{:02}-{day:02}", month + 1), "tokens":0, "level":0}));
            }
        }
        for day in 1..=6 {
            days.push(
                serde_json::json!({"date":format!("2026-01-{day:02}"), "tokens":100, "level":4}),
            );
        }
        let activity = serde_json::json!({"type":"metrics", "snapshot":{
            "kind":"activity", "snapshot":{"scope":"all", "days":days, "summary":vec!["Fixture figure"; 8]}
        }});
        let insights = serde_json::json!({"type":"metrics", "snapshot": {
            "kind":"insights", "markdown":"# Insights\n\n| Tool | Calls |\n| read | 7 |\n"
        }});
        super::ingest_remote(&daemon, &session, &insights)?;
        assert!(
            matches!(&daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["insights"],
            crate::model::EngineMetrics::Insights { markdown, .. } if markdown.contains("read | 7"))
        );
        let mut with_charts = insights.clone();
        with_charts["snapshot"]["charts"] =
            serde_json::json!([{"kind":"tools","rows":[["read",7]],"note":""}]);
        super::ingest_remote(&daemon, &session, &with_charts)?;
        assert!(
            matches!(&daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id].metrics["insights"],
            crate::model::EngineMetrics::Insights { charts, .. } if charts.len() == 1 && charts[0].rows[0].1 == 7.0)
        );
        for charts in [
            serde_json::json!([{"kind":"failure_rates","rows":[["bash",101]],"note":""}]),
            serde_json::json!([{"kind":"tools","rows":[["read",-1]],"note":""}]),
            serde_json::json!([{"kind":"tools","rows":[["read",0.5]],"note":""}]),
            serde_json::json!([{"kind":"tools","rows":[["read",7]],"note":"bad\ntext"}]),
            serde_json::json!(vec![
                serde_json::json!({"kind":"tools","rows":[["read",7]],"note":""});
                2
            ]),
        ] {
            let mut invalid = with_charts.clone();
            invalid["snapshot"]["charts"] = charts;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        for text in [
            "x".repeat(64 * 1024 + 1),
            "bad\0report".into(),
            "bad\u{1b}report".into(),
        ] {
            let mut invalid = insights.clone();
            invalid["snapshot"]["markdown"] = text.into();
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        super::ingest_remote(&daemon, &session, &activity)?;
        let current = daemon.snapshot(Some(&session.id), 0)?;
        assert!(
            matches!(&current.telemetry[&session.id].metrics["activity"], crate::model::EngineMetrics::Activity { snapshot } if snapshot.days.len() == 371)
        );
        for (field, value) in [
            ("scope", serde_json::json!("invalid")),
            ("days", serde_json::json!([])),
            ("summary", serde_json::json!(vec!["x"; 9])),
            ("summary", serde_json::json!(vec!["bad\nlabel"; 8])),
        ] {
            let mut invalid = activity.clone();
            invalid["snapshot"]["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        for (field, value) in [
            ("level", serde_json::json!(5)),
            ("level", serde_json::json!(1)),
            ("date", serde_json::json!("not-a-date")),
            ("date", serde_json::json!("2025-02-30")),
            ("tokens", serde_json::json!(-1)),
            ("tokens", serde_json::json!(1)),
        ] {
            let mut invalid = activity.clone();
            invalid["snapshot"]["snapshot"]["days"][0][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let inventory = serde_json::json!({"type":"metrics", "snapshot":{"kind":"engines", "snapshot":{
            "page":1,"total":1,"current":"gemma4-e4b","ram":64000000000u64,"free_disk":100000000000u64,
            "ds4":false,"gemma":true,"engines":[{"name":"gemma4-e4b","version":1,"notes":"Gemma 4","state":"installed","detail":"installed","missing_bytes":null}]
        }}});
        super::ingest_remote(&daemon, &session, &inventory)?;
        let current = daemon.snapshot(Some(&session.id), 0)?;
        assert!(
            matches!(&current.telemetry[&session.id].metrics["engines"], crate::model::EngineMetrics::Engines { snapshot } if snapshot.engines[0].name == "gemma4-e4b")
        );
        for (field, value) in [
            ("page", serde_json::json!(0)),
            ("total", serde_json::json!(0)),
            ("total", serde_json::json!(2)),
            ("current", serde_json::json!("x".repeat(129))),
        ] {
            let mut invalid = inventory.clone();
            invalid["snapshot"]["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        for (field, value) in [
            ("state", serde_json::json!("unknown")),
            ("name", serde_json::json!("bad\nname")),
            ("notes", serde_json::json!("x".repeat(1025))),
        ] {
            let mut invalid = inventory.clone();
            invalid["snapshot"]["snapshot"]["engines"][0][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let downloads = serde_json::json!({"type":"downloads", "snapshot":{
            "pending":true, "cancel_requested":"keep", "state":{
                "set":"test-model", "version":3, "current":"main", "index":1, "of":3,
                "done_bytes":41, "total_bytes":100, "rate_bps":12_000_000, "phase":"downloading", "error":null
            }
        }});
        super::ingest_remote(&daemon, &session, &downloads)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        let state = snapshot.telemetry[&session.id]
            .downloads
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap();
        assert!(state.can_cancel());
        assert_eq!(state.fraction(), Some(0.41));
        assert!(
            !snapshot.telemetry[&session.id]
                .downloads
                .as_ref()
                .unwrap()
                .foreground
        );
        let mut foreground = downloads.clone();
        foreground["snapshot"]["foreground"] = true.into();
        foreground["snapshot"]["pending"] = false.into();
        foreground["snapshot"]["cancel_requested"] = serde_json::Value::Null;
        foreground["snapshot"]["state"]["version"] = 0.into();
        super::ingest_remote(&daemon, &session, &foreground)?;
        let current = daemon.snapshot(Some(&session.id), 0)?;
        assert!(
            current.telemetry[&session.id]
                .downloads
                .as_ref()
                .unwrap()
                .foreground
        );
        let mut invalid_scope = foreground.clone();
        invalid_scope["snapshot"]["pending"] = true.into();
        assert!(super::ingest_remote(&daemon, &session, &invalid_scope).is_err());
        invalid_scope["snapshot"]["pending"] = false.into();
        invalid_scope["snapshot"]["state"] = serde_json::Value::Null;
        assert!(super::ingest_remote(&daemon, &session, &invalid_scope).is_err());
        for (field, value) in [
            ("done_bytes", serde_json::json!(101)),
            ("index", serde_json::json!(4)),
            ("phase", serde_json::json!("unrecognized")),
            ("set", serde_json::json!("bad\nmodel")),
        ] {
            let mut invalid = downloads.clone();
            invalid["snapshot"]["state"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut terminal = downloads.clone();
        terminal["snapshot"]["state"]["phase"] = "staged".into();
        assert!(
            super::ingest_remote(&daemon, &session, &terminal).is_err(),
            "terminal jobs cannot have cancellation pending"
        );
        terminal["snapshot"]["cancel_requested"] = serde_json::Value::Null;
        super::ingest_remote(&daemon, &session, &terminal)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        let state = snapshot.telemetry[&session.id]
            .downloads
            .as_ref()
            .unwrap()
            .state
            .as_ref()
            .unwrap();
        assert!(!state.can_cancel());
        assert!(state.fraction().is_none());
        let cache = serde_json::json!({"type":"cache", "snapshot": {
            "page":1, "total":1, "summary":"total 1 MiB · 0 reclaimable", "rows":[{
                "fingerprint":"full-fingerprint", "depth":1, "label":"session Test",
                "detail":"Test project", "status":"1 MiB · hits 2", "pinned":true
            }]
        }});
        super::ingest_remote(&daemon, &session, &cache)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        let row = &snapshot.telemetry[&session.id].cache.as_ref().unwrap().rows[0];
        assert_eq!(
            row.command("unpin").as_deref(),
            Some("/kvcache unpin full-fingerprint")
        );
        assert!(row.command("unknown").is_none());
        for (field, value) in [
            ("page", serde_json::json!(0)),
            ("total", serde_json::json!(2)),
        ] {
            let mut invalid = cache.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        for (field, value) in [
            ("fingerprint", serde_json::json!("bad\n/quit")),
            ("depth", serde_json::json!(65)),
            ("label", serde_json::json!("\u{1b}[31m")),
        ] {
            let mut invalid = cache.clone();
            invalid["snapshot"]["rows"][0][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut duplicate = cache.clone();
        duplicate["snapshot"]["total"] = 2.into();
        duplicate["snapshot"]["rows"] = serde_json::json!([
            cache["snapshot"]["rows"][0].clone(),
            cache["snapshot"]["rows"][0].clone()
        ]);
        assert!(super::ingest_remote(&daemon, &session, &duplicate).is_err());
        let left = "a".repeat(40);
        let right = "b".repeat(40);
        duplicate["snapshot"]["rows"][0]["entry"] = left.clone().into();
        duplicate["snapshot"]["rows"][1]["entry"] = right.clone().into();
        super::ingest_remote(&daemon, &session, &duplicate)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id].cache.as_ref().unwrap().rows[1].command("rm"),
            Some(format!("/kvcache rm --entry {right} full-fingerprint"))
        );
        let mut bad_handle = duplicate.clone();
        bad_handle["snapshot"]["rows"][1]["entry"] = left.into();
        assert!(super::ingest_remote(&daemon, &session, &bad_handle).is_err());
        for value in [
            serde_json::json!("bad\n/quit"),
            serde_json::json!("A".repeat(40)),
        ] {
            let mut invalid = duplicate.clone();
            invalid["snapshot"]["rows"][0]["entry"] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let extensions = serde_json::json!({"type":"extensions", "snapshot":{
            "kind":"skills", "report":"review · Review project\n", "enabled":true
        }});
        super::ingest_remote(&daemon, &session, &extensions)?;
        let mut hooks = extensions.clone();
        hooks["snapshot"]["kind"] = "hooks".into();
        hooks["snapshot"]["report"] = "PreToolUse: check".into();
        super::ingest_remote(&daemon, &session, &hooks)?;
        hooks["snapshot"]["enabled"] = false.into();
        super::ingest_remote(&daemon, &session, &hooks)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(snapshot.telemetry[&session.id].extensions.len(), 2);
        assert_eq!(
            snapshot.telemetry[&session.id].extensions["skills"].enabled,
            Some(true)
        );
        assert_eq!(
            snapshot.telemetry[&session.id].extensions["hooks"].enabled,
            Some(false)
        );
        for (field, value) in [
            ("kind", serde_json::json!("unknown")),
            ("kind", serde_json::json!("mcp")),
            ("report", serde_json::json!("x".repeat(65 * 1024 + 1))),
            ("report", serde_json::json!("\u{1b}[31m")),
        ] {
            let mut invalid = extensions.clone();
            invalid["snapshot"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        super::ingest_remote(&daemon, &session, &reset)?;
        super::ingest_remote(&daemon, &session, &reset)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert!(snapshot.telemetry.is_empty());
        assert_eq!(
            snapshot
                .entries
                .iter()
                .filter(|entry| entry.text.contains("replaced its conversation"))
                .count(),
            1
        );
        daemon.store.lock().expect("store poisoned").invalidate(
            &session.id,
            crate::model::RunState::Idle,
            false,
        )?;
        super::ingest_remote(&daemon, &session, &status)?;
        super::ingest_remote(&daemon, &session, &jobs)?;
        super::ingest_remote(&daemon, &session, &saved)?;
        super::ingest_remote(&daemon, &session, &branches)?;
        super::ingest_remote(&daemon, &session, &checkpoints)?;
        super::ingest_remote(&daemon, &session, &extensions)?;
        assert!(daemon.snapshot(Some(&session.id), 0)?.telemetry.is_empty());
        super::ingest_remote(&daemon, &session, &cache)?;
        assert!(daemon.snapshot(Some(&session.id), 0)?.telemetry.is_empty());
        super::ingest_remote(&daemon, &session, &downloads)?;
        assert!(daemon.snapshot(Some(&session.id), 0)?.telemetry.is_empty());
        for frame in [&context, &usage, &speed, &host] {
            super::ingest_remote(&daemon, &session, frame)?;
        }
        assert!(daemon.snapshot(Some(&session.id), 0)?.telemetry.is_empty());
        let replacement = daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&session.id)?;
        super::ingest_remote(&daemon, &replacement, &tasks)?;
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert!(snapshot.telemetry[&session.id].status.is_none());
        assert!(snapshot.telemetry[&session.id].jobs.is_none());
        assert!(snapshot.telemetry[&session.id].saved_sessions.is_none());
        assert!(snapshot.telemetry[&session.id].config.is_none());
        assert!(snapshot.telemetry[&session.id].branches.is_none());
        assert!(snapshot.telemetry[&session.id].checkpoints.is_none());
        assert!(snapshot.telemetry[&session.id].extensions.is_empty());
        assert!(snapshot.telemetry[&session.id].cache.is_none());
        assert!(snapshot.telemetry[&session.id].downloads.is_none());
        assert!(snapshot.telemetry[&session.id].metrics.is_empty());
        assert!(snapshot.telemetry[&session.id].host.is_none());
        assert_eq!(snapshot.telemetry[&session.id].tasks, Some((2, 5)));
        super::ingest_remote(
            &daemon,
            &replacement,
            &serde_json::json!({"type":"tasks", "completed":0, "total":0}),
        )?;
        assert!(
            daemon.snapshot(Some(&session.id), 0)?.telemetry[&session.id]
                .task_report
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn remote_commands_finish_only_on_the_matching_completion() {
        let done = serde_json::json!({"type":"command_done","text":"/usage"});
        assert!(super::remote_finished(&done, " /usage ", None));
        assert!(!super::remote_finished(&done, "/context", None));
        assert!(!super::remote_finished(
            &done,
            "write a test",
            Some("own-id")
        ));
        let notify = serde_json::json!({"type":"notify"});
        assert!(!super::remote_finished(&notify, "/usage", None));
        assert!(!super::remote_finished(
            &notify,
            "write a test",
            Some("own-id")
        ));
        let own = serde_json::json!({"type":"prompt_done","request_id":"own-id"});
        assert!(super::remote_finished(&own, "write a test", Some("own-id")));
        assert!(!super::remote_finished(
            &own,
            "write a test",
            Some("other-id")
        ));
        assert!(!super::remote_finished(&own, "write a test", None));
        let mut capabilities = super::RemoteInputCaps {
            commands: true,
            prompts: false,
            identified_commands: true,
            ownership_fencing: true,
        };
        assert!(super::remote_payload("write a test", capabilities).is_err());
        capabilities.prompts = true;
        capabilities.identified_commands = false;
        assert!(super::remote_payload("/command", capabilities).is_err());
        capabilities.identified_commands = true;
        capabilities.ownership_fencing = false;
        assert!(super::remote_payload("/command", capabilities).is_err());
        assert!(super::remote_payload("write a test", capabilities).is_err());
        let command = serde_json::json!({"type":"command_request_done","request_id":"own-id"});
        assert!(super::remote_finished(&command, "/command", Some("own-id")));
        assert!(!super::remote_finished(
            &command,
            "/command",
            Some("other-id")
        ));
    }

    #[test]
    fn repeated_engine_question_ids_receive_distinct_requirement_revisions() -> anyhow::Result<()> {
        let request = serde_json::json!({"id":"q1","header":"Question","question":"Continue?","multi":false,"options":[{"label":"Yes"}]}).to_string();
        let first = super::plank_question(&request, 0)?;
        let second = super::plank_question(&request, 0)?;
        assert_eq!(first.id, second.id);
        assert_ne!(first.requirement, second.requirement);
        Ok(())
    }

    #[test]
    fn remote_editor_replies_preserve_text_and_never_become_model_prompts() -> anyhow::Result<()> {
        let request =
            serde_json::json!({"id":"re1", "title":"File", "text":"original\n"}).to_string();
        let editor = super::plank_editor(&request, 7)?;
        assert_eq!(editor.requirement["kind"], "plank_editor");
        assert_eq!(editor.requirement["generation"], 7);
        assert_eq!(editor.arguments, "original\n");
        let saved = super::remote_answer(&editor, &serde_json::to_string(&Some("café\r\n🦀\n"))?)?;
        assert_eq!(saved["type"], "edit");
        assert_eq!(saved["document_id"], "re1");
        assert_eq!(saved["text"], "café\r\n🦀\n");
        let cancelled = super::remote_answer(&editor, "null")?;
        assert!(cancelled.get("text").unwrap().is_null());
        assert!(super::remote_answer(&editor, "{}").is_err());
        assert!(
            super::remote_answer(
                &editor,
                &serde_json::to_string(&Some("x".repeat(256 * 1024 + 1)))?
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn remote_question_forms_close_only_the_matching_request_and_fence_replaced_workers()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "PLANK_REMOTE_TOKEN".into(),
                },
            )?
        };
        let session = daemon.store.lock().expect("store poisoned").session(&id)?;
        let question = serde_json::json!({"type":"question", "request":{"id":"rq1","header":"Choice", "question":"Choose?", "multi":true,
            "options":[{"label":"One"},{"label":"Two"}]}});
        super::ingest_remote(&daemon, &session, &question)?;
        let approval = daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&id)?
            .approvals[0]
            .clone();
        let answer = super::remote_answer(&approval, "1,2")?;
        assert_eq!(answer["type"], "answer");
        assert_eq!(answer["question_id"], "rq1");
        assert_eq!(answer["answer"], "1,2");
        assert!(super::remote_answer(&approval, "4").is_err());
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"question_done", "question_id":"older"}),
        )?;
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .len(),
            1
        );
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"question_done", "question_id":"rq1"}),
        )?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .is_empty()
        );
        daemon.store.lock().expect("store poisoned").reset(&id)?;
        super::ingest_remote(&daemon, &session, &question)?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn conversation_reset_retires_old_controls_once_without_erasing_newer_views()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let session = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Local", directory.path().to_str().unwrap())?;
            let id = store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Local {
                    executable: "unused".into(),
                    args: vec![],
                },
            )?;
            store.session(&id)?
        };
        let publish = |label: &str| -> anyhow::Result<crate::model::PendingApproval> {
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"controls","controls":{
                    "version":1,"commands":[{"name":"/new","args":"","description":"New conversation","source":"builtin"}],
                    "suggestion":{"revision":label,"text":format!("Suggestion {label}")}
                }}),
            )?;
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"question","request":{
                    "id":label,"header":"Confirm","question":"Continue?","options":[{"label":"Yes"}]
                }}),
            )?;
            super::ingest_remote(
                &daemon,
                &session,
                &serde_json::json!({"type":"tasks","completed":0,"total":0,"report":label}),
            )?;
            Ok(daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&session.id)?
                .approvals[0]
                .clone())
        };
        let old = publish("old")?;
        let reset = serde_json::json!({"type":"reset","id":101});
        super::ingest_remote(&daemon, &session, &reset)?;
        let current = daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&session.id)?;
        assert!(current.approvals.is_empty());
        let controls = current.controls.unwrap();
        assert_eq!(controls.commands[0].name, "/new");
        assert!(controls.suggestion.is_none());
        assert!(
            !daemon
                .snapshot(Some(&session.id), 0)?
                .telemetry
                .contains_key(&session.id)
        );
        assert!(
            daemon
                .execute(crate::model::Command::DecideApproval {
                    session_id: session.id.clone(),
                    approval_id: old.id,
                    requirement: old.requirement,
                    choice_id: "1".into(),
                })
                .is_err()
        );
        publish("new")?;
        super::ingest_remote(&daemon, &session, &reset)?;
        let current = daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&session.id)?;
        assert_eq!(current.approvals[0].id, "new");
        assert_eq!(
            current.controls.unwrap().suggestion.unwrap().revision,
            "new"
        );
        let snapshot = daemon.snapshot(Some(&session.id), 0)?;
        assert_eq!(
            snapshot.telemetry[&session.id].task_report.as_deref(),
            Some("new")
        );
        assert_eq!(
            snapshot
                .entries
                .iter()
                .filter(|entry| entry.text.contains("Plank replaced its conversation"))
                .count(),
            1
        );
        let mut stale = session.clone();
        stale.generation += 1;
        super::ingest_remote(
            &daemon,
            &stale,
            &serde_json::json!({"type":"reset","id":102}),
        )?;
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&session.id)?
                .approvals[0]
                .id,
            "new"
        );
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"reset","id":102}),
        )?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&session.id)?
                .approvals
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn remote_catalog_updates_are_validated_and_fenced_after_reset() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "PLANK_REMOTE_TOKEN".into(),
                },
            )?
        };
        let session = daemon.store.lock().expect("store poisoned").session(&id)?;
        let catalog = serde_json::json!({"type":"controls", "controls":{"version":1,"commands":[{
            "name":"/project-review", "args":"<task>", "description":"Review project", "source":"skill"
        }]}});
        super::ingest_remote(&daemon, &session, &catalog)?;
        let mut updated = catalog.clone();
        updated["controls"]["commands"][0]["requirement"] = "Requires a project".into();
        updated["controls"]["features"] = serde_json::json!([{
            "name":"wasm", "label":"WASM runtime", "available":false,
            "detail":"Not compiled in this engine build."
        }]);
        updated["controls"]["settings"] = serde_json::json!({
            "model":"Echo", "temperature":0.75, "power_percent":0,
            "speculative":false, "speculation_available":false,
            "micro_compaction":true, "loop_guard":true, "steering":null
        });
        super::ingest_remote(&daemon, &session, &updated)?;
        let mut template = updated.clone();
        template["controls"]["commands"][0]["source"] = "template".into();
        template["controls"]["commands"][0]["parameters"] = serde_json::json!(["path", "topic"]);
        super::ingest_remote(&daemon, &session, &template)?;
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .controls
                .unwrap()
                .commands[0]
                .parameters
                .as_ref()
                .unwrap(),
            &["path", "topic"]
        );
        for parameters in [
            serde_json::json!(["path", "path"]),
            serde_json::json!(["bad=value"]),
            serde_json::json!(["x".repeat(65)]),
            serde_json::json!(vec!["x"; 33]),
        ] {
            let mut invalid = template.clone();
            invalid["controls"]["commands"][0]["parameters"] = parameters;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = template.clone();
        invalid["controls"]["commands"][0]["source"] = "skill".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .controls
                .unwrap()
                .settings
                .unwrap()
                .temperature,
            0.75
        );
        for invalid_features in [
            serde_json::json!([{"name":"bad\nname", "label":"WASM", "available":true, "detail":""}]),
            serde_json::json!([{"name":"wasm", "label":"WASM", "available":true, "detail":"bad\ntext"}]),
            serde_json::json!([
                updated["controls"]["features"][0].clone(),
                updated["controls"]["features"][0].clone()
            ]),
        ] {
            let mut invalid = updated.clone();
            invalid["controls"]["features"] = invalid_features;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        let mut invalid = updated.clone();
        invalid["controls"]["commands"][0]["requirement"] = "bad\nrequirement".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        for (field, value) in [
            ("temperature", serde_json::json!(-0.1)),
            ("power_percent", serde_json::json!(101)),
            ("speculative", serde_json::json!(true)),
            ("model", serde_json::json!("bad\nmodel")),
        ] {
            let mut invalid = updated.clone();
            invalid["controls"]["settings"][field] = value;
            assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        }
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .controls
                .unwrap()
                .commands[0]
                .name,
            "/project-review"
        );
        let mut invalid = catalog.clone();
        invalid["controls"]["commands"][0]["name"] = "not a command".into();
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        daemon.store.lock().expect("store poisoned").reset(&id)?;
        super::ingest_remote(&daemon, &session, &catalog)?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .controls
                .is_none()
        );
        Ok(())
    }

    #[tokio::test]
    async fn remote_editor_saves_negotiate_bounded_streams_without_changing_turn_completion()
    -> anyhow::Result<()> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            WebSocketStream,
            tungstenite::{Message, protocol::Role},
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            for streaming in [false, true] {
                let (daemon, session, _directory) = remote_restart_fixture()?;
                let body = "🦀\u{0}\\\r\n".repeat(if streaming { 131_072 } else { 20_000 });
                let expected = body.clone();
                let (saved, wait_saved) = tokio::sync::oneshot::channel();
                let (finish, wait_finish) = tokio::sync::oneshot::channel();
                let (client, server) = tokio::io::duplex(16 * 1024);
                let peer = tokio::spawn(async move {
                    let mut socket = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
                    let auth: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(auth["editor_stream"], true);
                    socket.send(Message::Text(
                        serde_json::json!({"type":"hello","protocol_version":1,"controller":true,"instance_id":"editor-host","command_completion":true,"command_request_completion":true,"ownership_fencing":true})
                        .to_string().into())).await?;
                    if streaming {
                        socket.send(Message::Text(serde_json::json!({"type":"replay","entry":{"id":1,"type":"visible","text":"Earlier reply"}}).to_string().into())).await?;
                        assert!(tokio::time::timeout(std::time::Duration::from_millis(25), socket.next()).await.is_err(), "Input must wait for the final snapshot marker");
                    }
                    socket.send(Message::Text(serde_json::json!({"type":"snapshot","scrollback":[]}).to_string().into())).await?;
                    let command: serde_json::Value = serde_json::from_str(socket.next().await.unwrap()?.to_text()?)?;
                    assert_eq!(command["text"], "/open example.rs");
                    socket.send(Message::Text(serde_json::json!({"type":"controls","controls":{"version":1,"commands":[],"features":[{"name":"editor_stream_save","label":"Chunked Saves","available":streaming,"detail":"Fixture"}]}}).to_string().into())).await?;
                    if streaming {
                        let original = "🦀\u{0}\\\r\n".repeat(20_000);
                        for frame in crate::documenttransfer::Sender::new("opening-1", &original).unwrap() {
                            socket.send(Message::Text(serde_json::json!({"type":"editor_stream","request":{"id":"edit-1","title":"Example","frame":frame}}).to_string().into())).await?;
                        }
                    } else {
                        socket.send(Message::Text(serde_json::json!({"type":"editor","request":{"id":"edit-1","title":"Example","text":"original"}}).to_string().into())).await?;
                    }
                    let mut receiver = crate::documenttransfer::Receiver::default();
                    let mut frames = 0;
                    loop {
                        let payload = socket.next().await.unwrap()?;
                        let value: serde_json::Value = serde_json::from_str(payload.to_text()?)?;
                        assert_eq!(value["type"], "edit");
                        assert_eq!(value["document_id"], "edit-1");
                        frames += 1;
                        if !streaming {
                            assert_eq!(value["text"].as_str(), Some(expected.as_str()));
                            break;
                        }
                        assert!(payload.len() < crate::documenttransfer::CHUNK_BYTES * 6 + 1024);
                        let frame: crate::documenttransfer::Frame = serde_json::from_value(value["text"].clone())?;
                        if frames == 1 {
                            assert!(matches!(frame, crate::documenttransfer::Frame::Begin { .. }));
                            socket.send(Message::Text(serde_json::json!({"type":"visible","text":"Host remains responsive"}).to_string().into())).await?;
                        }
                        if let Some(text) = receiver.accept(&frame).map_err(anyhow::Error::msg)? {
                            assert!(text == expected, "Chunked remote Save must preserve all bytes");
                            break;
                        }
                    }
                    assert!(if streaming { frames > 3 } else { frames == 1 });
                    saved.send(()).unwrap();
                    wait_finish.await?;
                    for frame in [
                        serde_json::json!({"type":"editor_done","document_id":"edit-1"}),
                        serde_json::json!({"type":"command_request_done","request_id":command["request_id"]}),
                        serde_json::json!({"type":"bye","reason":"fixture complete"}),
                    ] { socket.send(Message::Text(frame.to_string().into())).await?; }
                    Ok::<_, anyhow::Error>(())
                });
                let (tx, mut rx) = tokio::sync::mpsc::channel(2);
                let worker = tokio::spawn({
                    let daemon = std::sync::Arc::clone(&daemon);
                    let session = session.clone();
                    async move {
                        let socket = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
                        super::remote_socket(daemon, &session, socket, "secret", Some("/open example.rs"), &mut rx).await
                    }
                });
                let approval = loop {
                    let current = daemon.store.lock().unwrap().session(&session.id)?;
                    if let Some(approval) = current.approvals.first() { break approval.clone(); }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                };
                tx.send(super::Input::Approval { approval, choice: serde_json::to_string(&Some(&body))? }).await?;
                wait_saved.await?;
                let current = daemon.store.lock().unwrap().session(&session.id)?;
                assert_eq!(current.state, crate::model::RunState::Running);
                assert_eq!(current.approvals.len(), 1);
                assert!(!worker.is_finished());
                finish.send(()).unwrap();
                worker.await??;
                peer.await??;
                let snapshot = daemon.snapshot(Some(&session.id), 0)?;
                assert!(snapshot.sessions[0].approvals.is_empty());
                assert!(!snapshot.entries.iter().any(|entry| entry.text.contains(&body)));
                if streaming {
                    assert!(snapshot.entries.iter().any(|entry| entry.text.contains("Host remains responsive")));
                }
            }
            Ok::<_, anyhow::Error>(())
        }).await?
    }

    #[test]
    fn remote_editor_frames_close_only_the_current_document_and_ignore_replaced_workers()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let daemon =
            crate::daemon::Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = {
            let store = daemon.store.lock().expect("store poisoned");
            let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
            store.create_session(
                &workspace,
                "Plank",
                crate::model::Backend::Remote {
                    url: "ws://127.0.0.1:9100".into(),
                    token_env: "PLANK_REMOTE_TOKEN".into(),
                },
            )?
        };
        let session = daemon.store.lock().expect("store poisoned").session(&id)?;
        let request = serde_json::json!({"type":"editor", "request":{"id":"re1", "title":"Remote file", "text":"original\n"}});
        super::ingest_remote(&daemon, &session, &request)?;
        let pending = daemon
            .store
            .lock()
            .expect("store poisoned")
            .session(&id)?
            .approvals[0]
            .clone();
        assert_eq!(pending.requirement["kind"], "plank_editor");
        assert!(pending.arguments.is_empty());
        assert_eq!(
            daemon.store.lock().unwrap().document_chunk(
                &session.id,
                &pending.id,
                &pending.requirement,
                0
            )?,
            b"original\n"
        );
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"editor_done", "document_id":"older"}),
        )?;
        assert_eq!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .len(),
            1
        );
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"editor_done", "document_id":"re1"}),
        )?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .is_empty()
        );
        let mut invalid = request.clone();
        invalid["request"]["text"] = serde_json::Value::Null;
        assert!(super::ingest_remote(&daemon, &session, &invalid).is_err());
        daemon.store.lock().expect("store poisoned").reset(&id)?;
        super::ingest_remote(&daemon, &session, &request)?;
        super::ingest_remote(
            &daemon,
            &session,
            &serde_json::json!({"type":"editor_done", "document_id":"re1"}),
        )?;
        assert!(
            daemon
                .store
                .lock()
                .expect("store poisoned")
                .session(&id)?
                .approvals
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn utf8_survives_pipe_chunk_boundaries() {
        let mut pending = vec![0xe2, 0x82];
        assert_eq!(decode_utf8(&mut pending), "");
        pending.extend([0xac, b'!']);
        assert_eq!(decode_utf8(&mut pending), "€!");
        assert!(pending.is_empty());
    }
}
