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

/// Discards incomplete export files on errors, disconnection and task abort.
struct ExportLease {
    daemon: std::sync::Weak<Daemon>,
    session: String,
    generation: u64,
}

impl Drop for ExportLease {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.upgrade()
            && let Ok(mut artifacts) = daemon.artifacts.lock()
        {
            artifacts.discard_pending(&self.session, self.generation);
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
    let mut child = tokio::process::Command::new(executable)
        .args(args)
        .args(["--ui", "console"])
        .env("NO_COLOR", "1")
        .env("PLANK_GUI_PROTOCOL", "1")
        .env("PLANK_GUI_EXPORT_STREAM", "1")
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Cannot start Plank; check the executable and engine arguments")?;
    let _process = crate::metadata::ProcessLease::new(&daemon, session, child.id());
    let mut stdin = child.stdin.take().context("Missing engine stdin")?;
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
    let mut stdout_open = true;
    let mut stderr_open = true;
    // Startup must reach the documented readiness marker rather than hanging forever.
    let startup = tokio::time::sleep(Duration::from_secs(180));
    tokio::pin!(startup);
    loop {
        tokio::select! {
            _ = &mut startup, if booting && !question => bail!("Plank did not announce console readiness within three minutes"),
            message = input.recv(), if ready || question => {
                match message {
                    Some(Input::Prompt(prompt)) if ready => {
                        stdin.write_all(prompt.as_bytes()).await?;
                        stdin.write_all(b"\n").await?;
                        stdin.flush().await?;
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
                        let reply = if approval.requirement["kind"] == "plank_editor" {
                            let text: Option<String> = serde_json::from_str(&choice)?;
                            serde_json::json!({"type":"dialog_reply", "id": approval.id, "text": text})
                        } else {
                            ensure!(approval.requirement["kind"] == "plank_question", "Wrong approval protocol");
                            serde_json::json!({"type":"dialog_reply", "id": approval.id, "answer": choice})
                        };
                        stdin.write_all(reply.to_string().as_bytes()).await?;
                        stdin.write_all(b"\n").await?;
                        stdin.flush().await?;
                        daemon.store.lock().expect("store poisoned")
                            .resolve_approval(&session.id, session.generation, &approval.id)?;
                        question = false;
                        startup.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(180));
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
                ensure!(stderr_line.len() <= 2 * 1024 * 1024, "Engine stderr line is too large");
                let line = std::str::from_utf8(&stderr_line)?;
                if line.trim() == "+DWARFSTAR_WAITING" {
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
                        stdin.write_all(prompt.as_bytes()).await?;
                        stdin.write_all(b"\n").await?;
                        stdin.flush().await?;
                        running = true;
                        ready = false;
                        state(&daemon, session, RunState::Running)?;
                    }
                } else if line.trim() == "+PLANK_BUSY" {
                    ensure!(!booting, "Plank announced remote work before console readiness");
                    ready = false;
                    running = true;
                    state(&daemon, session, RunState::Running)?;
                } else if line.trim() == "+PLANK_RESTARTING" {
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
                } else if let Some(telemetry) = line.trim().strip_prefix("+PLANK_TELEMETRY ") {
                    ensure!(telemetry.len() <= 2 * 1024 * 1024, "Engine telemetry is too large");
                    ingest_telemetry(&daemon, session, &serde_json::from_str(telemetry)?)?;
                } else if let Some(progress) = line.trim().strip_prefix("+PLANK_PROGRESS ") {
                    ensure!(progress.len() <= 4096, "Engine progress is too large");
                    daemon.store.lock().expect("store poisoned").engine_progress(&session.id, session.generation, serde_json::from_str(progress)?)?;
                } else if let Some(done) = line.trim().strip_prefix("+PLANK_QUESTION_DONE ").or_else(|| line.trim().strip_prefix("+PLANK_EDITOR_DONE ")) {
                    ensure!(done.len() <= 1024, "Question completion is too large");
                    let done: serde_json::Value = serde_json::from_str(done)?;
                    if question && done["id"].as_str() == Some(question_id.as_str()) {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                        question = false;
                        startup.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(180));
                    }
                } else if let Some(catalog) = line.trim().strip_prefix("+PLANK_CONTROLS ") {
                    ensure!(catalog.len() <= 1024 * 1024, "Engine catalog is too large");
                    let controls = serde_json::from_str(catalog)?;
                    daemon.store.lock().expect("store poisoned")
                        .controls(&session.id, session.generation, controls)?;
                } else if let Some(request) = line.trim().strip_prefix("+PLANK_EDITOR ") {
                    let editor = plank_editor(request, session.generation)?;
                    if question {
                        daemon.store.lock().expect("store poisoned").resolve_approval(&session.id, session.generation, &question_id)?;
                    }
                    question_id.clone_from(&editor.id);
                    daemon.store.lock().expect("store poisoned").approval(&session.id, session.generation, editor)?;
                    question = true;
                } else if let Some(request) = line.trim().strip_prefix("+PLANK_QUESTION ") {
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
            exit = child.wait() => bail!("Plank exited ({}) and its live context was lost", exit?),
        }
    }
}

fn plank_editor(text: &str, generation: u64) -> Result<crate::model::PendingApproval> {
    ensure!(text.len() <= 2 * 1024 * 1024, "Editor request is too large");
    let request: serde_json::Value = serde_json::from_str(text)?;
    let text = request["text"].as_str().context("Missing document text")?;
    ensure!(
        text.len() <= 256 * 1024,
        "Document exceeds the editing limit"
    );
    let id = request["id"].as_str().context("Missing editor ID")?;
    ensure!(!id.is_empty() && id.len() <= 256, "Invalid editor ID");
    let title = request["title"].as_str().unwrap_or("Document");
    ensure!(title.len() <= 4096, "Editor title is too large");
    Ok(crate::model::PendingApproval {
        id: id.into(),
        requirement: serde_json::json!({"kind":"plank_editor", "generation":generation, "revision":uuid::Uuid::new_v4().to_string()}),
        tool: title.into(),
        arguments: text.into(),
        choices: Vec::new(),
    })
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
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(url),
    )
    .await??;
    remote_socket(daemon, session, socket, &token, prompt, input).await
}

async fn remote_socket<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send>(
    daemon: Arc<Daemon>,
    session: &Session,
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    token: &str,
    prompt: Option<&str>,
    input: &mut mpsc::Receiver<Input>,
) -> Result<()> {
    socket
        .send(Message::Text(
            serde_json::json!({"v":1,"type":if prompt.is_some() {"auth"} else {"observe_auth"},"token":token,"export_stream":true})
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
    let capabilities = RemoteInputCaps::from_hello(&hello);
    // Authenticate and consume the initial replay before submitting new input.
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(10), socket.next())
            .await?
            .context("Remote closed before snapshot")??;
        if !frame.is_text() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(frame.to_text()?)?;
        ingest_remote(&daemon, session, &value)?;
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
        return Ok(());
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
    loop {
        let frame = tokio::select! {
            frame = socket.next() => frame.context("Remote disconnected; inspect its session before retrying")??,
            command = input.recv() => {
                match command {
                    Some(Input::Prompt(prompt)) => {
                        ensure!(active.is_none() && pending.is_none(), "Remote already has an active or queued input");
                        if !remote_generation_current(&daemon, session)? {
                            socket.close(None).await?;
                            return Ok(());
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
                    Some(Input::Observe) => continue,
                    Some(Input::RemoteControl(request)) => {
                        ensure!(active.is_none() && (request || pending.is_none()), "Cannot release control while input is active or waiting");
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
                        return Ok(());
                    }
                    Some(Input::Approval { approval, choice }) => {
                        ensure!(access.controller, "Remote approval requires control");
                        let current = daemon.store.lock().expect("store poisoned").session(&session.id)?;
                        if current.generation != session.generation || !current.approvals.iter().any(|pending| pending.id == approval.id && pending.requirement == approval.requirement) {
                            continue;
                        }
                        socket.send(Message::Text(remote_answer(&approval, &choice)?.to_string().into())).await?;
                        continue;
                    }
                    None => {
                        ensure!(active.is_none(), "Remote input closed while a run was active; inspect its session before retrying");
                        socket.close(None).await?;
                        return Ok(());
                    }
                }
            }
        };
        if frame.is_close() {
            bail!("Remote disconnected; the remote run may still be executing");
        }
        if !frame.is_text() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(frame.to_text()?)?;
        ingest_remote(&daemon, session, &value)?;
        if value["type"] == "control" {
            access.controller = value["controller"]
                .as_bool()
                .context("Invalid control role")?;
            access.requesting = false;
            access.reason = None;
            ensure!(
                access.controller || active.is_none(),
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
                    return Ok(());
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
            ensure!(
                active.is_none(),
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

fn ingest_remote(daemon: &Daemon, session: &Session, value: &serde_json::Value) -> Result<()> {
    if value["type"] == "aside" {
        return ingest_telemetry(daemon, session, value);
    }
    if matches!(
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
                | "reset"
                | "artifact"
        )
    ) {
        return ingest_telemetry(daemon, session, value);
    }
    if let Some(replay) = value["scrollback"].as_array() {
        for entry in replay {
            ingest_remote(daemon, session, entry)?;
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
        drop(artifacts);
        if let Some(reason) = retirement {
            store.append(
                &session.id,
                "system",
                &format!("Export transfer incomplete: {reason}. Request /export again."),
            )?;
        }
        if let Some(export) = completed {
            let mut telemetry = daemon.telemetry.lock().expect("telemetry poisoned");
            let entry = telemetry
                .entry(session.id.clone())
                .or_insert_with(|| (session.generation, Default::default()));
            if entry.0 != session.generation {
                *entry = (session.generation, Default::default());
            }
            if export.purpose == crate::model::ExportPurpose::Insights {
                entry.1.insights_export = Some(export);
            } else {
                entry.1.export = Some(export);
            }
        }
        return Ok(());
    }
    if value["type"] == "reset" {
        let mut store = daemon.store.lock().expect("store poisoned");
        if store.session(&session.id)?.generation == session.generation {
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
            let note =
                "Plank replaced its conversation; earlier messages are retained for reference.";
            if let Some(sequence) = value["id"].as_i64() {
                store.append_remote(&session.id, sequence, "system", note)?;
            } else {
                store.append(&session.id, "system", note)?;
            }
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
        assert_eq!(pending.arguments, "original\n");
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
