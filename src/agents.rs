//! Provider runners preserve conversation identities and normalize headless events.
//!
//! Muse uses its versioned stdio session host. Codex and Claude run one process
//! per turn, resuming the exact persisted provider identity on later prompts.

// Rust guideline compliant 2026-02-21
use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::mpsc,
};

use crate::{
    daemon::Daemon,
    model::{
        AgentProvider, ApprovalChoice, Backend, PendingApproval, ProviderInfo, RunState, Session,
    },
    workers::Input,
};

pub(crate) fn providers() -> Vec<ProviderInfo> {
    [
        AgentProvider::MuseCode,
        AgentProvider::Codex,
        AgentProvider::ClaudeCode,
    ]
    .into_iter()
    .map(|provider| ProviderInfo {
        provider,
        executable: resolve_executable(provider.executable()).map(|p| p.to_string_lossy().into()),
    })
    .collect()
}

fn resolve_executable(name: &str) -> Option<PathBuf> {
    let path = PathBuf::from(name);
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file().then_some(path);
    }
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        paths.extend([home.join(".local/bin"), home.join(".cargo/bin")]);
    }
    paths.extend([
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    paths
        .into_iter()
        .map(|p| {
            p.join(if cfg!(windows) && !name.ends_with(".exe") {
                format!("{name}.exe")
            } else {
                name.into()
            })
        })
        .find(|p| p.is_file())
}

fn mutate<T>(
    daemon: &Daemon,
    session: &Session,
    f: impl FnOnce(&crate::store::Store) -> Result<T>,
) -> Result<T> {
    let store = daemon.store.lock().expect("store poisoned");
    ensure!(
        store.session(&session.id)?.generation == session.generation,
        "Agent worker was replaced"
    );
    f(&store)
}

fn append(daemon: &Daemon, session: &Session, role: &str, text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    mutate(daemon, session, |store| {
        store.append(&session.id, role, text)
    })
}

fn state(daemon: &Daemon, session: &Session, state: RunState) -> Result<()> {
    mutate(daemon, session, |store| {
        store.set_state(&session.id, state, false)
    })
}

// A malformed provider cannot allocate an unbounded newline-delimited frame.
async fn line(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8(bytes)?))
            };
        }
        let end = chunk.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let count = end.unwrap_or(chunk.len());
        ensure!(
            bytes.len() + count <= 2 * 1024 * 1024,
            "Provider frame exceeds 2 MiB"
        );
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(String::from_utf8(bytes)?));
        }
    }
}

fn process(executable: &str, args: &[String], directory: &str) -> Result<Child> {
    let executable = resolve_executable(executable).context(
        "Agent executable was not found on the daemon host; install it or provide an absolute path",
    )?;
    Ok(tokio::process::Command::new(executable)
        .args(args)
        .env("NO_COLOR", "1")
        .env("MUSE_NO_AUTO_UPDATE", "1")
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?)
}

pub(crate) async fn run(
    daemon: Arc<Daemon>,
    session: &Session,
    directory: &str,
    mut input: mpsc::Receiver<Input>,
) -> Result<()> {
    let Backend::Agent {
        provider,
        executable,
        args,
        model,
    } = &session.backend
    else {
        bail!("Wrong agent backend");
    };
    if *provider == AgentProvider::MuseCode {
        return muse(daemon, session, directory, &mut input).await;
    }
    while let Some(Input::Prompt(prompt)) = input.recv().await {
        let identity = mutate(&daemon, session, |store| {
            Ok(store.session(&session.id)?.agent_session)
        })?;
        let mut command = if *provider == AgentProvider::Codex {
            vec!["exec".into()]
        } else {
            vec![
                "--print".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
            ]
        };
        command.extend(args.clone());
        if let Some(model) = model.as_ref().filter(|m| !m.trim().is_empty()) {
            command.extend(["--model".into(), model.clone()]);
        }
        if *provider == AgentProvider::Codex {
            if identity.is_some() {
                command.push("resume".into());
            }
            command.extend(["--json".into(), "--skip-git-repo-check".into()]);
        }
        if let Some(identity) = identity {
            if *provider == AgentProvider::ClaudeCode {
                command.push("--resume".into());
            }
            command.push(identity);
        }
        if *provider == AgentProvider::Codex {
            command.push("-".into());
        }
        let mut child = process(executable, &command, directory)?;
        let _process = crate::metadata::ProcessLease::new(&daemon, session, child.id());
        let mut stdin = child.stdin.take().context("Agent stdin unavailable")?;
        stdin.write_all(prompt.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);
        state(&daemon, session, RunState::Running)?;
        let mut stdout = BufReader::new(child.stdout.take().context("Agent stdout unavailable")?);
        let mut stderr = BufReader::new(child.stderr.take().context("Agent stderr unavailable")?);
        let (mut out_open, mut err_open) = (true, true);
        let mut complete = false;
        while out_open || err_open {
            tokio::select! {
                output = line(&mut stdout), if out_open => match output? {
                    Some(output) if !output.trim().is_empty() => {
                        let event: Value = serde_json::from_str(&output).context("Agent output is not valid JSONL")?;
                        complete |= cli_event(&daemon, session, *provider, &event)?;
                    }
                    Some(_) => {}, None => out_open = false,
                },
                output = line(&mut stderr), if err_open => match output? {
                    Some(output) => append(&daemon, session, "diagnostic", &output)?, None => err_open = false,
                },
            }
        }
        let exit = child.wait().await?;
        ensure!(
            exit.success(),
            "{} exited with {exit}; inspect its diagnostics",
            provider.label()
        );
        ensure!(
            complete,
            "{} exited without a successful turn completion",
            provider.label()
        );
        state(&daemon, session, RunState::Idle)?;
    }
    Ok(())
}

fn cli_event(
    daemon: &Daemon,
    session: &Session,
    provider: AgentProvider,
    value: &Value,
) -> Result<bool> {
    let kind = value["type"].as_str().unwrap_or("");
    if provider == AgentProvider::Codex {
        if kind == "thread.started" {
            let id = value["thread_id"]
                .as_str()
                .context("Codex omitted thread_id")?;
            mutate(daemon, session, |store| {
                store.agent_identity(&session.id, session.generation, id, true)
            })?;
        }
        if kind == "turn.failed" || kind == "error" {
            bail!(
                "Codex: {}",
                value["error"]
                    .as_object()
                    .and_then(|e| e.get("message"))
                    .or_else(|| value.get("message"))
                    .unwrap_or(value)
            );
        }
        if kind == "item.completed" {
            let item = &value["item"];
            let role = match item["type"].as_str().unwrap_or("") {
                "agent_message" => "assistant",
                "reasoning" => "thinking",
                _ => "tool",
            };
            let text = item["text"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| serde_json::to_string_pretty(item).unwrap_or_default());
            append(daemon, session, role, &text)?;
        }
        return Ok(kind == "turn.completed");
    }
    if let Some(id) = value["session_id"].as_str() {
        mutate(daemon, session, |store| {
            store.agent_identity(&session.id, session.generation, id, true)
        })?;
    }
    if kind == "assistant"
        && let Some(content) = value["message"]["content"].as_array()
    {
        for block in content {
            let (role, field) = match block["type"].as_str().unwrap_or("") {
                "text" => ("assistant", "text"),
                "thinking" => ("thinking", "thinking"),
                _ => ("tool", ""),
            };
            let text = block[field]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| block.to_string());
            append(daemon, session, role, &text)?;
        }
    }
    if kind == "user"
        && let Some(content) = value["message"]["content"].as_array()
    {
        for block in content.iter().filter(|b| b["type"] == "tool_result") {
            append(daemon, session, "tool", &block["content"].to_string())?;
        }
    }
    if kind == "result" {
        ensure!(
            value["is_error"] != true && value["subtype"] == "success",
            "Claude failed: {}",
            value
        );
        return Ok(true);
    }
    Ok(false)
}

#[derive(Debug)]
struct MuseHost {
    _child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    streamed: HashMap<String, String>,
    kinds: HashMap<String, String>,
}

impl MuseHost {
    async fn send(&mut self, value: &Value) -> Result<()> {
        self.stdin.write_all(value.to_string().as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await?;
        Ok(id)
    }
    async fn rpc(
        &mut self,
        daemon: &Daemon,
        session: &Session,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.request(method, params).await?;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let value = self.read().await?;
                if value["id"] == id {
                    ensure!(
                        value.get("error").is_none(),
                        "Muse {method}: {}",
                        value["error"]
                    );
                    return Ok(value["result"].clone());
                }
                self.event(daemon, session, &value).await?;
            }
        })
        .await
        .context("Muse did not answer its startup request")?
    }
    async fn read(&mut self) -> Result<Value> {
        let line = line(&mut self.stdout)
            .await?
            .context("Muse session host disconnected")?;
        serde_json::from_str(&line).context("Muse emitted invalid JSON-RPC")
    }
    async fn event(&mut self, daemon: &Daemon, session: &Session, value: &Value) -> Result<bool> {
        ensure!(
            value.get("error").is_none(),
            "Muse command failed: {}",
            value["error"]
        );
        let params = &value["params"];
        match value["method"].as_str().unwrap_or("") {
            "item/started" => {
                if let (Some(id), Some(kind)) = (
                    params["item"]["itemId"].as_str(),
                    params["item"]["kind"].as_str(),
                ) {
                    self.kinds.insert(id.into(), kind.into());
                }
            }
            "item/delta" => {
                let id = params["itemId"]
                    .as_str()
                    .context("Muse delta has no identity")?;
                let field = params["field"].as_str().unwrap_or("text");
                if let Some(delta) = params["delta"].as_str() {
                    let role = muse_role(self.kinds.get(id).map(String::as_str).unwrap_or(
                        if field == "text" {
                            "agentMessage"
                        } else {
                            "toolCall"
                        },
                    ));
                    append(daemon, session, role, delta)?;
                    self.streamed
                        .entry(format!("{id}:{field}"))
                        .or_default()
                        .push_str(delta);
                }
            }
            "item/completed" => {
                let item = &params["item"];
                let kind = item["kind"].as_str().unwrap_or("unknown");
                if kind != "userMessage" {
                    let id = item["itemId"].as_str().unwrap_or("");
                    let field = if kind == "toolCall" { "output" } else { "text" };
                    let text = item[if field == "output" {
                        "visibleOutput"
                    } else {
                        field
                    }]
                    .as_str()
                    .or_else(|| item["fallbackText"].as_str())
                    .unwrap_or("");
                    let streamed = self
                        .streamed
                        .remove(&format!("{id}:{field}"))
                        .unwrap_or_default();
                    append(
                        daemon,
                        session,
                        muse_role(kind),
                        text.strip_prefix(&streamed).unwrap_or(text),
                    )?;
                    self.kinds.remove(id);
                }
            }
            "approval/request" | "approval/requested" => {
                let approval = PendingApproval {
                    document: None,
                    id: params["approvalId"]
                        .as_str()
                        .context("Missing approval identity")?
                        .into(),
                    requirement: params["currentRequirementId"].clone(),
                    tool: params["toolName"].as_str().unwrap_or("Tool").into(),
                    arguments: params["rawArgs"].as_str().unwrap_or("").into(),
                    choices: params["availableChoices"]
                        .as_array()
                        .context("Missing approval choices")?
                        .iter()
                        .filter_map(|v| {
                            Some(ApprovalChoice {
                                id: v["choiceId"].as_str()?.into(),
                                label: v["label"].as_str()?.into(),
                            })
                        })
                        .collect(),
                };
                mutate(daemon, session, |store| {
                    store.approval(&session.id, session.generation, approval)
                })?;
                if let Some(id) = value.get("id") {
                    self.send(&json!({"jsonrpc":"2.0","id":id,"result":{}}))
                        .await?;
                }
            }
            "approval/resolved" => {
                if let Some(id) = params["approvalId"].as_str() {
                    mutate(daemon, session, |store| {
                        store.resolve_approval(&session.id, session.generation, id)
                    })?;
                }
            }
            "userInput/request" => bail!(
                "Muse requested a structured user-input dialog; stop or resume this conversation in Muse to answer it"
            ),
            "turn/completed" => {
                ensure!(
                    params["terminal"] == "completed",
                    "Muse turn ended {}: {}",
                    params["terminal"],
                    params["reason"]
                );
                state(daemon, session, RunState::Idle)?;
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
    }
}

fn muse_role(kind: &str) -> &'static str {
    match kind {
        "agentMessage" => "assistant",
        "reasoning" => "thinking",
        _ => "tool",
    }
}

async fn muse(
    daemon: Arc<Daemon>,
    session: &Session,
    directory: &str,
    input: &mut mpsc::Receiver<Input>,
) -> Result<()> {
    let Backend::Agent {
        executable,
        args,
        model,
        ..
    } = &session.backend
    else {
        bail!("Wrong Muse backend");
    };
    let mut command = vec!["serve".into()];
    command.extend(args.clone());
    if let Some(model) = model.as_ref().filter(|m| !m.is_empty()) {
        command.extend(["--model".into(), model.clone()]);
    }
    let mut child = process(executable, &command, directory)?;
    let _process = crate::metadata::ProcessLease::new(&daemon, session, child.id());
    let mut stderr = BufReader::new(child.stderr.take().context("Muse stderr unavailable")?);
    let mut host = MuseHost {
        stdin: child.stdin.take().context("Muse stdin unavailable")?,
        stdout: BufReader::new(child.stdout.take().context("Muse stdout unavailable")?),
        _child: child,
        next_id: 0,
        streamed: HashMap::new(),
        kinds: HashMap::new(),
    };
    let hello = host
        .rpc(
            &daemon,
            session,
            "initialize",
            json!({"clientInfo":{"name":"nplank", "version":env!("CARGO_PKG_VERSION")}}),
        )
        .await?;
    ensure!(
        hello["schema"]["version"] == 1,
        "Unsupported Muse protocol version"
    );
    host.send(&json!({"jsonrpc":"2.0","method":"initialized","params":{}}))
        .await?;
    let resumable = hello["sessionDurability"] != "ephemeral";
    let saved = mutate(&daemon, session, |store| {
        Ok(store.session(&session.id)?.agent_session)
    })?;
    let identity = if let Some(saved) = saved {
        host.rpc(&daemon, session, "session/resume", json!({"commandId":uuid::Uuid::now_v7().to_string(), "sessionId":saved, "excludeItems":true})).await?["session"]["sessionId"].as_str().context("Muse resume identity missing")?.to_owned()
    } else {
        host.rpc(
            &daemon,
            session,
            "session/start",
            json!({"commandId":uuid::Uuid::now_v7().to_string(), "workspaceRoot":directory}),
        )
        .await?["session"]["sessionId"]
            .as_str()
            .context("Muse session identity missing")?
            .to_owned()
    };
    mutate(&daemon, session, |store| {
        store.agent_identity(&session.id, session.generation, &identity, resumable)
    })?;
    let mut stderr_open = true;
    loop {
        tokio::select! {
            value = host.read() => { host.event(&daemon, session, &value?).await?; },
            output = line(&mut stderr), if stderr_open => match output? { Some(output) => append(&daemon, session, "diagnostic", &output)?, None => stderr_open = false },
            command = input.recv() => match command {
                Some(Input::Prompt(prompt)) => {
                    host.streamed.clear(); host.kinds.clear();
                    host.request("turn/start", json!({"commandId":uuid::Uuid::now_v7().to_string(), "sessionId":identity, "ifBusy":"queue", "input":[{"type":"text", "text":prompt}]})).await?;
                    state(&daemon, session, RunState::Running)?;
                }
                Some(Input::Approval { approval, choice }) => {
                    host.request("approval/decide", json!({"commandId":uuid::Uuid::now_v7().to_string(), "sessionId":identity, "approvalId":approval.id, "requirementId":approval.requirement, "choiceId":choice})).await?;
                }
                Some(Input::Stop) | None => return Ok(()),
                Some(Input::Observe | Input::RemoteControl(_) | Input::Aside(_) | Input::LiveControl(_) | Input::FrameControl(_) | Input::StopJobOutput(_)) => bail!("Plank remote controls cannot be sent to Muse"),
            }
        }
    }
}
