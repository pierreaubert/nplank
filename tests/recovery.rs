//! Recovery and transport tests exercise real process and HTTP boundaries.

// Rust guideline compliant 2026-02-21
use anyhow::Result;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use nplank::{
    daemon::{Daemon, router},
    model::{Backend, Command, RunState},
    store::Store,
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;

fn demo(daemon: &Arc<Daemon>, directory: &std::path::Path) -> Result<String> {
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Workspace".into(),
            directory: directory.to_string_lossy().into(),
        })?
        .id;
    Ok(daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Session".into(),
            backend: Backend::Demo,
        })?
        .id)
}

fn prompt(id: &str, request: &str, text: &str) -> Command {
    Command::Prompt {
        session_id: id.into(),
        request_id: request.into(),
        text: text.into(),
        images: Vec::new(),
    }
}

#[cfg(feature = "test-engine")]
fn local_fixture_session(
    daemon: &Arc<Daemon>,
    directory: &std::path::Path,
    args: Vec<String>,
) -> Result<String> {
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Local".into(),
            directory: directory.to_string_lossy().into(),
        })?
        .id;
    Ok(daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args,
            },
        })?
        .id)
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn framed_console_prompts_preserve_multiline_and_literal_protocol_objects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(&daemon, directory.path(), vec![])?;
    let inputs = [
        "First line\nSecond 🦀 line\nThird line",
        "{\"type\":\"dialog_reply\",\"id\":\"old\",\"answer\":\"1\"}",
        "{\"type\":\"btw\",\"text\":\"literal JSON\"}",
        "/remember first line\nsecond line",
    ];
    for (index, text) in inputs.into_iter().enumerate() {
        daemon.execute(prompt(&id, &format!("framed-{index}"), text))?;
        wait_idle(&daemon, &id).await?;
    }
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let output: String = snapshot
        .entries
        .iter()
        .filter(|entry| entry.role == "assistant")
        .map(|entry| entry.text.as_str())
        .collect();
    for (index, text) in inputs.into_iter().enumerate() {
        assert!(
            output.contains(&format!("turn {}: {text} €", index + 1)),
            "{output}"
        );
    }
    assert!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .approvals
            .is_empty()
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn legacy_console_prompt_transport_remains_usable_without_the_capability() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(&daemon, directory.path(), vec!["--legacy-prompts".into()])?;
    daemon.execute(prompt(&id, "legacy", "Legacy text stays text"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.role == "assistant"
                && entry.text.contains("turn 1: Legacy text stays text"))
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn intentional_console_exit_preserves_history_and_starts_a_fresh_engine() -> Result<()> {
    for command in ["/quit", "/exit"] {
        let directory = tempfile::tempdir()?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = local_fixture_session(&daemon, directory.path(), vec![])?;
        daemon.execute(prompt(&id, "first", "before exit"))?;
        wait_idle(&daemon, &id).await?;
        daemon.execute(prompt(&id, "quit", command))?;
        wait_idle(&daemon, &id).await?;
        let snapshot = daemon.snapshot(Some(&id), 0)?;
        let session = snapshot
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap();
        assert_eq!(session.generation, 1);
        assert!(!session.recovery_required);
        assert!(session.controls.is_none());
        assert!(!snapshot.telemetry.contains_key(&id));
        assert!(
            snapshot
                .entries
                .iter()
                .any(|message| message.text.contains("Final output before exit"))
        );
        assert!(
            !snapshot
                .entries
                .iter()
                .any(|message| message.role == "error")
        );
        daemon.execute(prompt(&id, "next", "after exit"))?;
        wait_idle(&daemon, &id).await?;
        let snapshot = daemon.snapshot(Some(&id), 0)?;
        assert!(
            snapshot
                .entries
                .iter()
                .any(|message| message.text.contains("after exit"))
        );
        daemon.execute(Command::Stop { session_id: id })?;
    }
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn console_exit_without_acknowledgment_or_success_still_requires_recovery() -> Result<()> {
    for argument in ["--exit-without-marker", "--fail-exit"] {
        let directory = tempfile::tempdir()?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let id = local_fixture_session(&daemon, directory.path(), vec![argument.into()])?;
        daemon.execute(prompt(&id, "quit", "/quit"))?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = daemon.snapshot(Some(&id), 0)?;
                if snapshot
                    .sessions
                    .iter()
                    .any(|session| session.id == id && session.state == RunState::Failed)
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        let snapshot = daemon.snapshot(Some(&id), 0)?;
        assert!(
            snapshot
                .sessions
                .iter()
                .any(|session| session.id == id && session.recovery_required)
        );
    }
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn local_side_questions_use_typed_frames_while_preserving_a_running_main_turn() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Local".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec!["--busy-aside".into()],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "main-1", "main-with-aside"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if snapshot.sessions[0].can_receive_asides()
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.text.contains("Main prefix"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    daemon.execute(prompt(&id, "side-1", "/btw Question 1?"))?;
    daemon.execute(prompt(&id, "side-1", "/btw Question 1?"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if !snapshot.asides.is_empty() {
                assert_eq!(snapshot.asides.len(), 1);
                assert_eq!(snapshot.asides[0].text, "Side answer 1");
                assert_eq!(snapshot.sessions[0].state, RunState::Running);
                assert!(
                    !snapshot
                        .entries
                        .iter()
                        .any(|entry| entry.text.contains("Question 1?"))
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    assert!(
        daemon
            .execute(prompt(&id, "main-2", "Other main task"))
            .is_err()
    );
    daemon.execute(prompt(&id, "side-2", "/btw Question 2?"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(snapshot.asides.len(), 2);
    let main = snapshot
        .entries
        .iter()
        .filter(|entry| entry.role == "assistant")
        .map(|entry| entry.text.as_str())
        .collect::<String>();
    assert_eq!(main, "Main prefix main continuation.\n");
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.role == "user")
            .count(),
        1
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn live_controls_update_settings_without_completing_the_main_turn() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Local".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec!["--busy-live-control".into()],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "main-live", "main-with-live-controls"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if snapshot.sessions[0].can_receive_live_controls()
                && snapshot
                    .entries
                    .iter()
                    .any(|entry| entry.text.contains("Live control main started"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    let before = daemon.snapshot(Some(&id), 0)?;
    daemon.execute(prompt(&id, "live-mc", "/mc off"))?;
    daemon.execute(prompt(&id, "live-mc", "/mc off"))?;
    assert!(daemon.execute(prompt(&id, "live-mc", "/mc on")).is_err());
    assert!(
        daemon
            .execute(prompt(&id, "live-invalid", "/mc off /quit"))
            .is_err()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if daemon.snapshot(Some(&id), 0)?.sessions[0]
                .controls
                .as_ref()
                .and_then(|controls| controls.settings.as_ref())
                .is_some_and(|settings| !settings.micro_compaction)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    daemon.execute(prompt(&id, "live-lg", "/lg off"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match daemon.execute(prompt(&id, "live-hints", "/hints on")) {
                Ok(_) => break,
                Err(error) => assert!(
                    error.to_string().contains("cannot accept a live setting"),
                    "{error:#}"
                ),
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    daemon.execute(prompt(&id, "live-hints", "/hints on"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            let session = &snapshot.sessions[0];
            if session
                .controls
                .as_ref()
                .and_then(|controls| controls.settings.as_ref())
                .is_some_and(|settings| {
                    !settings.micro_compaction
                        && !settings.loop_guard
                        && settings.hints == Some(true)
                })
            {
                assert_eq!(session.state, RunState::Running);
                assert_eq!(session.generation, before.sessions[0].generation);
                assert_eq!(snapshot.entries, before.entries);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    daemon.execute(prompt(&id, "live-notify", "/notify never"))?;
    daemon.execute(prompt(&id, "live-notify", "/notify never"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if snapshot.sessions[0]
                .controls
                .as_ref()
                .and_then(|controls| controls.settings.as_ref())
                .is_some_and(|settings| {
                    settings.notify_mode == Some(nplank::livecontrol::NotifyMode::Never)
                })
                && !snapshot.notifications.is_empty()
            {
                assert_eq!(snapshot.notifications.len(), 1);
                assert_eq!(snapshot.notifications[0].title, "Local completion");
                assert_eq!(snapshot.sessions[0].state, RunState::Running);
                assert_eq!(snapshot.entries, before.entries);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    std::fs::write(directory.path().join("release-live-control"), b"release")?;
    wait_idle(&daemon, &id).await?;
    daemon.execute(prompt(&id, "after-live", "/commands"))?;
    wait_idle(&daemon, &id).await?;
    let after = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(
        after
            .entries
            .iter()
            .filter(|entry| entry.role == "user")
            .count(),
        2
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
async fn wait_job_output(
    daemon: &Daemon,
    id: &str,
    following: bool,
) -> Result<nplank::joboutput::Page> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(id), 0)?;
            if let Some(nplank::model::EngineMetrics::JobOutput { snapshot: page }) = snapshot
                .telemetry
                .get(id)
                .and_then(|value| value.metrics.get("job_output"))
                && page.following == following
            {
                return Ok::<_, anyhow::Error>(page.clone());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn job_output_cancel_reaches_busy_local_worker_without_prompt_or_interruption() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Local".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec!["--busy-job-output".into()],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "main-output", "main-with-output"))?;
    let page = wait_job_output(&daemon, &id, true).await?;
    let generation = daemon.snapshot(Some(&id), 0)?.sessions[0].generation;
    let stop = |generation, observer: &str| Command::StopJobOutput {
        session_id: id.clone(),
        generation,
        observer: observer.into(),
    };
    assert!(
        daemon
            .execute(stop(generation + 1, "fixture-observer"))
            .is_err()
    );
    assert!(daemon.execute(stop(generation, "stale-observer")).is_err());
    assert!(
        daemon
            .execute(stop(generation, "invalid observer"))
            .is_err()
    );
    daemon.execute(stop(generation, page.observer.as_deref().unwrap()))?;
    let stopped = wait_job_output(&daemon, &id, false).await?;
    assert_eq!(stopped.observer, page.observer);
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(snapshot.sessions[0].state, RunState::Running);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.role == "user")
            .count(),
        1
    );
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("job_output_stop"))
    );
    assert!(
        daemon
            .execute(stop(generation, "fixture-observer"))
            .is_err()
    );
    std::fs::write(directory.path().join("release-main"), [])?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("completed after explicit release"))
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
async fn wait_focus_update(daemon: &Daemon, id: &str, expected: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            if daemon
                .snapshot(Some(id), 0)?
                .entries
                .iter()
                .any(|entry| entry.text.contains(expected))
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn gui_focus_reaches_busy_local_engine_and_expires_after_client_disconnect() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Local".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec!["--focus-control".into()],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "focus-main", "main-with-focus"))?;
    wait_focus_update(&daemon, &id, "Focus update 1: false").await?;
    let generation = daemon.snapshot(Some(&id), 0)?.sessions[0].generation;
    let client = uuid::Uuid::new_v4().to_string();
    let focus = |generation| Command::ViewFocus {
        client_id: client.clone(),
        session_id: Some(id.clone()),
        generation,
    };
    assert!(daemon.execute(focus(generation + 1)).is_err());
    daemon.execute(focus(generation))?;
    wait_focus_update(&daemon, &id, "Focus update 2: true").await?;
    wait_focus_update(&daemon, &id, "Focus update 3: false").await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(snapshot.sessions[0].state, RunState::Running);
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.role == "user")
            .count(),
        1
    );
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("view_focus"))
    );
    std::fs::write(directory.path().join("release-focus"), [])?;
    wait_idle(&daemon, &id).await?;
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(all(feature = "test-engine", feature = "gui"))]
#[tokio::test]
async fn graphical_buddy_local_transport_keeps_pngs_out_of_history_and_retires_them_on_reset()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Buddy".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Agent".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec![],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "buddy-on", "/buddy on"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let frame = snapshot.telemetry[&id].buddy.as_ref().unwrap();
    assert!(frame.is_valid());
    assert_eq!(frame.dimensions(), Some((2, 2)));
    assert!(daemon.snapshot(None, 0)?.telemetry[&id].buddy.is_none());
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("IHDR") || entry.text.contains("\"png\""))
    );
    daemon.execute(prompt(&id, "buddy-off", "/buddy off"))?;
    wait_idle(&daemon, &id).await?;
    assert!(
        daemon.snapshot(Some(&id), 0)?.telemetry[&id]
            .buddy
            .is_none()
    );
    daemon.execute(prompt(&id, "buddy-again", "/buddy on"))?;
    wait_idle(&daemon, &id).await?;
    assert!(
        daemon.snapshot(Some(&id), 0)?.telemetry[&id]
            .buddy
            .is_some()
    );
    daemon.execute(Command::Stop {
        session_id: id.clone(),
    })?;
    daemon.execute(Command::Reset {
        session_id: id.clone(),
    })?;
    assert!(
        daemon
            .snapshot(Some(&id), 0)?
            .telemetry
            .get(&id)
            .is_none_or(|value| value.buddy.is_none())
    );
    Ok(())
}

async fn wait_idle(daemon: &Daemon, id: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let snapshot = daemon.snapshot(Some(id), 0)?;
            if snapshot
                .sessions
                .iter()
                .any(|s| s.id == id && s.state == RunState::Idle)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await??;
    Ok(())
}

#[cfg(feature = "test-engine")]
async fn wait_question(daemon: &Daemon, id: &str) -> Result<nplank::model::PendingApproval> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(question) = daemon
                .snapshot(Some(id), 0)?
                .sessions
                .iter()
                .find(|session| session.id == id)
                .and_then(|session| session.approvals.first())
                .cloned()
            {
                return Ok::<_, anyhow::Error>(question);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn console_editor_completion_closes_only_its_request_before_readiness() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Documents".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Plank".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: Vec::new(),
            },
        })?
        .id;
    daemon.execute(prompt(&id, "open", "/remote-editor"))?;
    let editor = wait_question(&daemon, &id).await?;
    let decision = Command::EditDocument {
        session_id: id.clone(),
        approval_id: editor.id,
        requirement: editor.requirement,
        text: Some("edited 🦀\n".into()),
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            let session = snapshot
                .sessions
                .iter()
                .find(|session| session.id == id)
                .unwrap();
            if session.approvals.is_empty() {
                assert_eq!(
                    session.state,
                    RunState::Running,
                    "editor completion must not release a still-busy engine"
                );
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(
        daemon.execute(decision).is_err(),
        "a late Save must not reach the closed editor"
    );
    assert!(
        daemon
            .execute(prompt(&id, "too-early", "not ready yet"))
            .is_err()
    );
    wait_idle(&daemon, &id).await?;
    assert_eq!(
        std::fs::read_to_string(directory.path().join("editor.txt"))?,
        "edited 🦀\n"
    );
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn remote_driven_console_work_blocks_local_prompts_until_its_question_finishes() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Remote host".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Plank".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: Vec::new(),
            },
        })?
        .id;
    daemon.execute(prompt(&id, "trigger", "/remote-turn"))?;
    let question = wait_question(&daemon, &id).await?;
    assert_eq!(question.id, "remote-question");
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .state,
        RunState::Running
    );
    assert!(
        daemon
            .execute(prompt(&id, "unsafe", "This must not become an answer"))
            .is_err()
    );
    daemon.execute(Command::DecideApproval {
        session_id: id.clone(),
        approval_id: question.id,
        requirement: question.requirement,
        choice_id: "1".into(),
    })?;
    wait_idle(&daemon, &id).await?;
    daemon.execute(prompt(&id, "later", "A normal later prompt"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("Remote turn answered explicitly"))
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("A normal later prompt"))
    );
    assert!(
        snapshot
            .sessions
            .iter()
            .find(|session| session.id == id)
            .unwrap()
            .approvals
            .is_empty()
    );
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn startup_questions_preserve_queued_prompts_and_engine_choices_survive_fresh_context()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Engines".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Plank".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec!["--startup-question".into(), "--fragmented-controls".into()],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "first", "first actual prompt"))?;
    let startup = wait_question(&daemon, &id).await?;
    assert_eq!(startup.id, "startup-1");
    daemon.execute(Command::DecideApproval {
        session_id: id.clone(),
        approval_id: startup.id,
        requirement: startup.requirement,
        choice_id: "1".into(),
    })?;
    wait_idle(&daemon, &id).await?;
    assert_eq!(
        daemon.snapshot(Some(&id), 0)?.telemetry[&id].summary(),
        "idle · Context 2048/8192 · Think med · Tasks 1/3 · Jobs 1"
    );
    assert_eq!(
        daemon.snapshot(Some(&id), 0)?.telemetry[&id]
            .jobs
            .as_ref()
            .unwrap()
            .jobs[0]
            .path,
        "/tmp/job-output"
    );
    assert!(
        daemon.snapshot(Some(&id), 0)?.telemetry[&id]
            .task_report
            .as_ref()
            .is_some_and(|report| report.contains("Build the UI"))
    );
    assert!(
        daemon
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .any(|entry| entry.text.contains("turn 1: first actual prompt"))
    );
    daemon.execute(prompt(&id, "engines", "/engines"))?;
    let engine = wait_question(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap();
    assert!(session.controls.is_none());
    assert_eq!(session.summary, "Downloading test-engine · 50%");
    assert_eq!(session.progress.as_ref().unwrap().current, 50);
    daemon.execute(Command::DecideApproval {
        session_id: id.clone(),
        approval_id: engine.id,
        requirement: engine.requirement,
        choice_id: "2".into(),
    })?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap();
    assert!(session.progress.is_none());
    assert!(session.approvals.is_empty());
    assert!(session.controls.is_some());
    assert!(
        matches!(&session.backend, Backend::Local { args, .. } if args == &["--model:new-engine"])
    );
    daemon.execute(prompt(&id, "download", "/auto-download"))?;
    let download = wait_question(&daemon, &id).await?;
    wait_idle(&daemon, &id).await?;
    assert!(
        daemon
            .execute(Command::DecideApproval {
                session_id: id.clone(),
                approval_id: download.id,
                requirement: download.requirement,
                choice_id: "1".into()
            })
            .is_err()
    );
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let session = snapshot
        .sessions
        .iter()
        .find(|session| session.id == id)
        .unwrap();
    assert!(session.approvals.is_empty());
    assert!(session.progress.is_none());
    daemon.execute(Command::Reset {
        session_id: id.clone(),
    })?;
    daemon.execute(prompt(&id, "next", "after switching"))?;
    wait_idle(&daemon, &id).await?;
    assert!(daemon.snapshot(Some(&id), 0)?.sessions.iter().any(|session| session.id == id && session.approvals.is_empty() && matches!(&session.backend, Backend::Local { args, .. } if args == &["--model:new-engine"])));
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[test]
fn transient_engine_progress_is_bounded_and_rejected_after_replacement() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace = store.create_workspace("Engines", directory.path().to_str().unwrap())?;
    let id = store.create_session(
        &workspace,
        "Plank",
        Backend::Local {
            executable: "plank".into(),
            args: vec![],
        },
    )?;
    let progress = nplank::model::EngineProgress {
        label: "Loading engine".into(),
        current: 0,
        total: 0,
    };
    store.engine_progress(&id, 0, progress.clone())?;
    assert_eq!(
        store.snapshot(Some(&id), 0)?.sessions[0].summary,
        "Loading engine"
    );
    store.engine_launch(&id, 0, vec!["--model:new-engine".into()])?;
    assert!(store.session(&id)?.progress.is_none());
    assert!(
        store
            .engine_launch(&id, 0, vec!["--ui".into(), "console".into()])
            .is_err()
    );
    let invalid = nplank::model::EngineProgress {
        current: 101,
        total: 100,
        ..progress.clone()
    };
    assert!(store.engine_progress(&id, 0, invalid).is_err());
    store.reset(&id)?;
    assert!(store.engine_progress(&id, 0, progress).is_err());
    assert!(store.engine_launch(&id, 0, vec![]).is_err());
    Ok(())
}

#[tokio::test]
async fn accepted_prompt_is_deduplicated_across_reconnects() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = demo(&daemon, directory.path())?;
    daemon.execute(prompt(&id, "request-1", "hello"))?;
    daemon.execute(prompt(&id, "request-1", "hello"))?;
    let error = daemon
        .execute(prompt(&id, "request-1", "different"))
        .expect_err("conflicting request must fail");
    assert!(error.to_string().contains("different content"));
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(
        snapshot.entries.iter().filter(|e| e.role == "user").count(),
        1
    );
    assert!(
        snapshot
            .entries
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    Ok(())
}

#[tokio::test]
async fn dropping_http_attachment_does_not_stop_execution() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = demo(&daemon, directory.path())?;
    let attachment = router(Arc::clone(&daemon));
    let request = Request::builder()
        .method("POST")
        .uri("/v1/command")
        .header("authorization", "Bearer secret")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&prompt(
            &id,
            "request-http",
            "continue after disconnect",
        ))?))?;
    let response = attachment.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::OK);
    drop(response);
    wait_idle(&daemon, &id).await?;
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .uri(format!("/v1/snapshot?session={id}"))
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    let snapshot: nplank::model::Snapshot =
        serde_json::from_slice(&axum::body::to_bytes(response.into_body(), 1024 * 1024).await?)?;
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.role == "assistant")
    );
    let unauthorized = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .uri("/v1/snapshot")
                .header("authorization", "Bearer wrong-token")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    Ok(())
}

#[test]
fn daemon_restart_never_replays_unfinished_tools() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("state.sqlite");
    let mut store = Store::open(&path)?;
    let workspace =
        store.create_workspace("Project", directory.path().to_str().expect("temp path"))?;
    let session = store.create_session(&workspace, "Agent", Backend::Demo)?;
    store.accept_prompt(&session, "pending", "run a tool")?;
    store.append(&session, "tool", "tool began")?;
    drop(store);
    let reopened = Store::open(&path)?;
    reopened.recover()?;
    let recovered = reopened.session(&session)?;
    assert_eq!(recovered.state, RunState::Interrupted);
    assert!(recovered.recovery_required);
    assert!(
        reopened
            .snapshot(Some(&session), 0)?
            .entries
            .iter()
            .any(|e| e.text == "tool began")
    );
    reopened.reset(&session)?;
    assert!(!reopened.session(&session)?.recovery_required);
    Ok(())
}

#[tokio::test]
async fn moving_sessions_preserves_transcripts_and_identity() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = demo(&daemon, directory.path())?;
    daemon.execute(prompt(&id, "move-test", "hello"))?;
    wait_idle(&daemon, &id).await?;
    let second = daemon
        .execute(Command::CreateWorkspace {
            name: "Second".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    daemon.execute(Command::MoveSession {
        session_id: id.clone(),
        workspace_id: second.clone(),
    })?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert_eq!(snapshot.sessions[0].workspace_id, second);
    assert!(snapshot.entries.iter().any(|e| e.text == "hello"));
    Ok(())
}

#[test]
fn remote_server_instances_scope_replay_ids_across_store_reopens_without_replaying_requests()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("state.sqlite");
    let mut store = Store::open(&database)?;
    let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
    let backend = Backend::Remote {
        url: "ws://127.0.0.1:9100".into(),
        token_env: "FIXTURE_TOKEN".into(),
    };
    let id = store.create_session(&workspace, "Agent", backend.clone())?;
    let other = store.create_session(&workspace, "Other", backend)?;
    let generation = store.session(&id)?.generation;
    store.accept_prompt(&id, "accepted-main", "Original request")?;
    store.set_state(&id, RunState::Idle, false)?;
    store.append_remote(&id, 1, "assistant", "legacy answer", None)?;
    store.append_remote(&other, 1, "assistant", "other session", None)?;
    assert!(!store.bind_remote_instance(&id, generation, None)?);
    store.append_remote(&id, 1, "assistant", "legacy duplicate", None)?;
    assert!(store.bind_remote_instance(&id, generation, Some("server-one"))?);
    store.append_remote(&id, 1, "assistant", "first instance", None)?;
    assert!(!store.bind_remote_instance(&id, generation, Some("server-one"))?);
    store.append_remote(&id, 1, "assistant", "same instance duplicate", None)?;
    drop(store);
    let mut store = Store::open(&database)?;
    assert!(!store.bind_remote_instance(&id, generation, Some("server-one"))?);
    store.append_remote(&id, 1, "assistant", "reopened duplicate", None)?;
    assert!(store.bind_remote_instance(&id, generation, Some("server-two"))?);
    store.append_remote(&id, 1, "assistant", "second instance", None)?;
    let snapshot = store.snapshot(Some(&id), 0)?;
    let answers = snapshot
        .entries
        .iter()
        .filter(|entry| entry.role == "assistant")
        .map(|entry| entry.text.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        answers,
        ["legacy answer", "first instance", "second instance"]
    );
    assert_eq!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.role == "system")
            .count(),
        2
    );
    assert_eq!(
        store.snapshot(Some(&other), 0)?.entries[0].text,
        "other session"
    );
    assert!(!store.accept_prompt(&id, "accepted-main", "Original request")?);
    assert_eq!(store.session(&id)?.generation, generation);
    Ok(())
}

#[test]
fn remote_instance_binding_refuses_stale_workers_and_malformed_ids_before_retiring_replay()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace = store.create_workspace("Remote", directory.path().to_str().unwrap())?;
    let id = store.create_session(
        &workspace,
        "Agent",
        Backend::Remote {
            url: "ws://127.0.0.1:9100".into(),
            token_env: "FIXTURE_TOKEN".into(),
        },
    )?;
    let old_generation = store.session(&id)?.generation;
    store.reset(&id)?;
    let generation = store.session(&id)?.generation;
    assert!(!store.bind_remote_instance(&id, generation, Some("current"))?);
    store.append_remote(&id, 1, "assistant", "current answer", None)?;
    assert!(
        store
            .bind_remote_instance(&id, old_generation, Some("stale"))
            .is_err()
    );
    for invalid in [
        String::new(),
        "bad identity".into(),
        "🦀".into(),
        "x".repeat(129),
    ] {
        assert!(
            store
                .bind_remote_instance(&id, generation, Some(&invalid))
                .is_err()
        );
    }
    store.append_remote(&id, 1, "assistant", "must remain a duplicate", None)?;
    assert_eq!(
        store
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .filter(|entry| entry.role == "assistant")
            .count(),
        1
    );
    assert!(store.bind_remote_instance(&id, generation, None)?);
    store.append_remote(&id, 1, "assistant", "legacy downgrade", None)?;
    assert!(!store.bind_remote_instance(&id, generation, None)?);
    store.append_remote(&id, 1, "assistant", "legacy duplicate", None)?;
    assert_eq!(
        store
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .filter(|entry| entry.role == "assistant")
            .count(),
        2
    );
    Ok(())
}

#[test]
fn remote_snapshot_replay_is_deduplicated_and_pages_are_bounded() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace =
        store.create_workspace("Project", directory.path().to_str().expect("temp path"))?;
    let id = store.create_session(&workspace, "Agent", Backend::Demo)?;
    store.append_remote(&id, 1, "assistant", "once", None)?;
    store.append_remote(&id, 1, "assistant", "once", None)?;
    assert_eq!(store.snapshot(Some(&id), 0)?.entries.len(), 1);
    for _ in 0..300 {
        store.append(&id, "assistant", "chunk")?;
    }
    let latest = store.snapshot(Some(&id), 0)?;
    assert_eq!(latest.entries.len(), 250);
    let older = store.snapshot(Some(&id), latest.entries[0].sequence)?;
    assert_eq!(older.entries.len(), 51);
    assert_eq!(older.entries[0].text, "once");
    Ok(())
}

#[test]
fn aside_history_pages_independently_without_displacing_main_turns() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace =
        store.create_workspace("Project", directory.path().to_str().expect("temp path"))?;
    let id = store.create_session(&workspace, "Agent", Backend::Demo)?;
    let other = store.create_session(&workspace, "Other", Backend::Demo)?;
    for index in 0..300 {
        store.append(&id, "assistant", &format!("main {index}"))?;
    }
    // More than a full page of side output must not evict any main entries.
    for index in 0..300 {
        let role = [
            "aside",
            "aside_thinking",
            "aside_tool",
            "aside_notice",
            "aside_error",
        ][index % 5];
        store.append(&id, role, &format!("side {index}"))?;
    }
    store.append(&other, "aside", "another session")?;
    let latest = store.snapshot_pages(Some(&id), 0, 0)?;
    assert_eq!(latest.entries.len(), 250);
    assert_eq!(latest.asides.len(), 250);
    assert_eq!(latest.entries[0].text, "main 50");
    assert_eq!(latest.asides[0].text, "side 50");
    assert!(latest.asides.iter().all(|entry| entry.session_id == id));
    let main_cursor = latest.entries[0].sequence;
    let aside_cursor = latest.asides[0].sequence;
    let older_main = store.snapshot_pages(Some(&id), main_cursor, 0)?;
    assert_eq!(older_main.entries.len(), 50);
    assert_eq!(older_main.entries[0].text, "main 0");
    assert_eq!(older_main.asides, latest.asides);
    let older_side = store.snapshot_pages(Some(&id), 0, aside_cursor)?;
    assert_eq!(older_side.entries, latest.entries);
    assert_eq!(older_side.asides.len(), 50);
    assert_eq!(older_side.asides[0].text, "side 0");
    let both = store.snapshot_pages(Some(&id), main_cursor, aside_cursor)?;
    assert_eq!(both.entries, older_main.entries);
    assert_eq!(both.asides, older_side.asides);
    assert!(store.snapshot_pages(None, 0, 0)?.asides.is_empty());
    Ok(())
}

#[tokio::test]
async fn stop_then_reset_prevents_old_worker_output_from_reappearing() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = demo(&daemon, directory.path())?;
    daemon.execute(prompt(&id, "old", "old run"))?;
    tokio::time::sleep(Duration::from_millis(120)).await;
    daemon.execute(Command::Stop {
        session_id: id.clone(),
    })?;
    daemon.execute(Command::Reset {
        session_id: id.clone(),
    })?;
    let boundary = daemon
        .snapshot(Some(&id), 0)?
        .entries
        .last()
        .expect("reset entry")
        .sequence;
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(
        daemon
            .snapshot(Some(&id), 0)?
            .entries
            .last()
            .expect("reset entry")
            .sequence,
        boundary
    );
    daemon.execute(prompt(&id, "new", "new run"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let later: String = snapshot
        .entries
        .iter()
        .filter(|e| e.sequence > boundary && e.role == "assistant")
        .map(|e| e.text.as_str())
        .collect();
    assert!(later.contains("new run"));
    assert!(!later.contains("old run"));
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn local_console_keeps_context_across_prompts_and_isolates_a_crash() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Project".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Local".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec![],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "local-1", "one"))?;
    wait_idle(&daemon, &id).await?;
    daemon.execute(prompt(&id, "local-2", "two"))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    let controls = snapshot.sessions[0]
        .controls
        .as_ref()
        .expect("Runtime engine catalog");
    assert!(
        controls
            .commands
            .iter()
            .any(|command| command.name == "/project-review" && command.source == "skill")
    );
    assert!(
        controls
            .commands
            .iter()
            .any(|command| command.name == "/subagent:research")
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("turn 2: two €"))
    );
    daemon.execute(prompt(&id, "local-crash", "crash"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if snapshot.sessions[0].state == RunState::Failed {
                assert!(snapshot.sessions[0].recovery_required);
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let second = demo(&daemon, directory.path())?;
    daemon.execute(prompt(&second, "survivor", "still alive"))?;
    wait_idle(&daemon, &second).await?;
    Ok(())
}

#[test]
fn runtime_catalog_accepts_only_advertised_builtin_shell_escapes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace = store.create_workspace("Shell", directory.path().to_str().unwrap())?;
    let id = store.create_session(&workspace, "Plank", Backend::Demo)?;
    let mut controls: nplank::commands::EngineControls = serde_json::from_value(
        serde_json::json!({
            "version":1,
            "features":[{"name":"shell_escapes","label":"Shell escapes","available":true,"detail":"Direct commands"}],
            "commands":[
                {"name":"!","args":"<command>","description":"Shell with context","source":"builtin"},
                {"name":"!!","args":"<command>","description":"Shell without context","source":"builtin"}
            ]
        }),
    )?;
    store.controls(&id, 0, controls.clone())?;
    assert_eq!(store.session(&id)?.controls.as_ref(), Some(&controls));
    assert_eq!(
        nplank::commands::matching_in(&controls.commands, "shell").len(),
        2
    );
    for name in ["!!!", "! echo unsafe", "!\n/quit", "shell"] {
        controls.commands[0].name = name.into();
        assert!(store.controls(&id, 0, controls.clone()).is_err(), "{name}");
    }
    controls.commands[0].name = "!".into();
    controls.commands[0].source = "template".into();
    assert!(store.controls(&id, 0, controls.clone()).is_err());
    controls.commands[0].source = "builtin".into();
    controls.features[0].available = false;
    assert!(store.controls(&id, 0, controls).is_err());
    Ok(())
}

#[test]
fn runtime_catalog_is_cleared_when_an_engine_is_replaced() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace = store.create_workspace("Catalog", directory.path().to_str().unwrap())?;
    let id = store.create_session(
        &workspace,
        "Plank",
        Backend::Local {
            executable: "plank".into(),
            args: vec![],
        },
    )?;
    let controls = nplank::commands::EngineControls {
        suggestion: Some(nplank::commands::PromptSuggestion {
            revision: "offer-1".into(),
            text: "Review the changes".into(),
        }),
        version: 1,
        settings: None,
        features: Vec::new(),
        commands: vec![nplank::commands::PlankCommand {
            name: "/custom".into(),
            args: "<task>".into(),
            description: "Project helper".into(),
            source: "skill".into(),
            requirement: None,
            parameters: None,
        }],
    };
    store.controls(&id, 0, controls.clone())?;
    assert_eq!(store.session(&id)?.controls.as_ref(), Some(&controls));
    store.reset(&id)?;
    assert!(store.session(&id)?.controls.is_none());
    assert!(store.controls(&id, 0, controls.clone()).is_err());
    let generation = store.session(&id)?.generation;
    let mut invalid = controls.clone();
    invalid.commands[0].name = "/custom\n/injected".into();
    assert!(store.controls(&id, generation, invalid).is_err());
    store.controls(&id, generation, controls.clone())?;
    store.set_state(&id, RunState::Failed, true)?;
    assert!(store.session(&id)?.controls.is_none());
    store.controls(&id, generation, controls)?;
    drop(store);
    let reopened = Store::open(&directory.path().join("state.sqlite"))?;
    reopened.recover()?;
    assert!(reopened.session(&id)?.controls.is_none());
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn plank_questions_wait_for_explicit_valid_answers_and_keep_the_engine_alive() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Questions".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Plank".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec![],
            },
        })?
        .id;
    daemon.execute(prompt(&id, "ask", "question"))?;
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = daemon.snapshot(Some(&id), 0)?;
            if let Some(approval) = snapshot.sessions[0].approvals.first() {
                assert_eq!(snapshot.sessions[0].state, RunState::Running);
                break Ok::<_, anyhow::Error>(approval.clone());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    let decision = |choice: &str, requirement| Command::DecideApproval {
        session_id: id.clone(),
        approval_id: approval.id.clone(),
        choice_id: choice.into(),
        requirement,
    };
    assert!(
        daemon
            .execute(decision("99", approval.requirement.clone()))
            .is_err()
    );
    assert!(
        daemon
            .execute(decision("0,1", approval.requirement.clone()))
            .is_err()
    );
    assert!(
        daemon
            .execute(decision("1", serde_json::json!({"generation": 999})))
            .is_err()
    );
    daemon.execute(Command::AcknowledgeSession {
        session_id: id.clone(),
    })?;
    assert_eq!(
        daemon.snapshot(Some(&id), 0)?.sessions[0].approvals.len(),
        1
    );
    daemon.execute(decision("1,2", approval.requirement.clone()))?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(snapshot.sessions[0].approvals.is_empty());
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("explicit answer: \"1,2\""))
    );
    assert!(
        daemon
            .execute(decision("1", approval.requirement.clone()))
            .is_err()
    );
    daemon.execute(prompt(&id, "continue", "/usage"))?;
    wait_idle(&daemon, &id).await?;
    assert!(
        daemon
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .any(|entry| entry.text.contains("turn 2: /usage"))
    );
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn editor_uploads_reject_unauthorized_stale_oversized_and_invalid_bodies() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, "original")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(&daemon, directory.path(), vec![])?;
    daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
    let editor = wait_question(&daemon, &id).await?;
    let uri = editor_upload_uri(&id, &editor, b"never read")?;
    for stale in [false, true] {
        let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let signal = Arc::clone(&polled);
        let body = Body::from_stream(futures_util::stream::once(async move {
            signal.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"never read"))
        }));
        let request_uri = if stale {
            let mut old = editor.clone();
            old.requirement["extra"] = "not-current".into();
            editor_upload_uri(&id, &old, b"never read")?
        } else {
            uri.clone()
        };
        let response = router(Arc::clone(&daemon))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(request_uri)
                    .header(
                        "authorization",
                        if stale {
                            "Bearer secret"
                        } else {
                            "Bearer wrong"
                        },
                    )
                    .header("content-type", "text/plain; charset=utf-8")
                    .body(body)?,
            )
            .await?;
        assert_eq!(
            response.status(),
            if stale {
                StatusCode::CONFLICT
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
        assert!(!polled.load(std::sync::atomic::Ordering::Relaxed));
    }
    // An older engine must refuse a declared large Save without polling its body.
    let polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let signal = Arc::clone(&polled);
    let mut legacy_uri = reqwest::Url::parse(&format!("http://127.0.0.1{uri}"))?;
    legacy_uri
        .query_pairs_mut()
        .clear()
        .append_pair("session", &id)
        .append_pair("document", &editor.id)
        .append_pair("requirement", &serde_json::to_string(&editor.requirement)?)
        .append_pair("bytes", &(256 * 1024 + 1).to_string())
        .append_pair("sha256", &"0".repeat(64));
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "{}?{}",
                    legacy_uri.path(),
                    legacy_uri.query().unwrap()
                ))
                .header("authorization", "Bearer secret")
                .header("content-type", "text/plain")
                .body(Body::from_stream(futures_util::stream::once(async move {
                    signal.store(true, std::sync::atomic::Ordering::Relaxed);
                    Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"never read"))
                })))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(!polled.load(std::sync::atomic::Ordering::Relaxed));
    assert!(
        daemon
            .execute(Command::EditDocument {
                session_id: id.clone(),
                approval_id: editor.id.clone(),
                requirement: editor.requirement.clone(),
                text: Some("x".repeat(256 * 1024 + 1))
            })
            .is_err()
    );
    for (mime, body, expected) in [
        (
            "application/json",
            b"unexpected".to_vec(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        ("text/plain", vec![0xff], StatusCode::BAD_REQUEST),
        (
            "text/plain",
            vec![b'x'; 256 * 1024 + 1],
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
    ] {
        let request_uri = editor_upload_uri(&id, &editor, &body[..body.len().min(256 * 1024)])?;
        let response = router(Arc::clone(&daemon))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(request_uri)
                    .header("authorization", "Bearer secret")
                    .header("content-type", mime)
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), expected);
        assert_eq!(std::fs::read_to_string(&path)?, "original");
    }
    for body in [b"short".as_slice(), b"xxxxxxxxxx".as_slice()] {
        let response = router(Arc::clone(&daemon))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&uri)
                    .header("authorization", "Bearer secret")
                    .header("content-type", "text/plain")
                    .body(Body::from(body))?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(std::fs::read_to_string(&path)?, "original");
    }
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(editor_upload_uri(&id, &editor, b"")?)
                .header("authorization", "Bearer secret")
                .header("content-type", "text/plain")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    wait_idle(&daemon, &id).await?;
    assert_eq!(
        std::fs::read_to_string(&path)?,
        "",
        "Empty text must Save rather than Cancel"
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn editor_uploads_revalidate_the_revision_after_reading_the_body() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, "original")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(&daemon, directory.path(), vec![])?;
    daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
    let editor = wait_question(&daemon, &id).await?;
    let reset_daemon = Arc::clone(&daemon);
    let reset_id = id.clone();
    let body = Body::from_stream(futures_util::stream::once(async move {
        reset_daemon
            .execute(Command::Reset {
                session_id: reset_id,
            })
            .unwrap();
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"stale document"))
    }));
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(editor_upload_uri(&id, &editor, b"stale document")?)
                .header("authorization", "Bearer secret")
                .header("content-type", "text/plain")
                .body(body)?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(std::fs::read_to_string(&path)?, "original");
    assert!(
        daemon.snapshot(Some(&id), 0)?.sessions[0]
            .approvals
            .is_empty()
    );
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn editor_uploads_accept_the_document_limit_without_json_expansion() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, "original")?;
    std::fs::write(directory.path().join("stream-ack"), "")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(&daemon, directory.path(), vec!["--editor-stream".into()])?;
    daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
    let editor = wait_question(&daemon, &id).await?;
    let text = "\u{0}".repeat(256 * 1024);
    let command = Command::EditDocument {
        session_id: id.clone(),
        approval_id: editor.id.clone(),
        requirement: editor.requirement.clone(),
        text: Some(text.clone()),
    };
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/command")
                .header("authorization", "Bearer secret")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&command)?))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // Exercise the exact bulk Save limit without building its expanded JSON body.
    let pattern = "🦀\u{0}\\\r\n";
    let text = pattern.repeat(nplank::documenttransfer::DOCUMENT_BYTES / pattern.len());
    assert_eq!(text.len(), nplank::documenttransfer::DOCUMENT_BYTES);
    let uri = editor_upload_uri(&id, &editor, text.as_bytes())?;
    let response = router(Arc::clone(&daemon))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", "Bearer secret")
                .header("content-type", "text/plain; charset=utf-8")
                .body(Body::from(text.clone()))?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    wait_idle(&daemon, &id).await?;
    assert!(
        std::fs::read_to_string(&path)? == text,
        "Upload must preserve every document byte"
    );
    assert!(
        daemon.snapshot(Some(&id), 0)?.sessions[0]
            .approvals
            .is_empty()
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
async fn read_editor_document(
    daemon: &Arc<Daemon>,
    id: &str,
    editor: &nplank::model::PendingApproval,
) -> Result<String> {
    let document = editor.document.as_ref().unwrap();
    assert!(
        editor.arguments.is_empty(),
        "Snapshots must not include document bodies"
    );
    let mut text = Vec::new();
    for offset in (0..document.bytes.max(1)).step_by(nplank::model::EDITOR_FETCH_BYTES) {
        let mut url = reqwest::Url::parse("http://127.0.0.1/v1/editor")?;
        url.query_pairs_mut()
            .append_pair("session", id)
            .append_pair("document", &editor.id)
            .append_pair("requirement", &serde_json::to_string(&editor.requirement)?)
            .append_pair("offset", &offset.to_string());
        let response = router(Arc::clone(daemon))
            .oneshot(
                Request::builder()
                    .uri(format!("{}?{}", url.path(), url.query().unwrap()))
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        text.extend_from_slice(
            &axum::body::to_bytes(response.into_body(), nplank::model::EDITOR_FETCH_BYTES).await?,
        );
    }
    assert_eq!(nplank::documenttransfer::digest(&text), document.sha256);
    Ok(String::from_utf8(text)?)
}

#[cfg(feature = "test-engine")]
fn editor_upload_uri(
    id: &str,
    editor: &nplank::model::PendingApproval,
    text: &[u8],
) -> Result<String> {
    let mut url = reqwest::Url::parse("http://127.0.0.1/v1/editor")?;
    url.query_pairs_mut()
        .append_pair("session", id)
        .append_pair("document", &editor.id)
        .append_pair("requirement", &serde_json::to_string(&editor.requirement)?)
        .append_pair("bytes", &text.len().to_string())
        .append_pair("sha256", &nplank::documenttransfer::digest(text));
    Ok(format!("{}?{}", url.path(), url.query().unwrap()))
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn local_editor_opening_streams_ignore_completion_after_the_editor_closes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, "original")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(
        &daemon,
        directory.path(),
        vec!["--editor-open-stream".into(), "--editor-open-cancel".into()],
    )?;
    daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.path().join("opening-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    std::fs::write(directory.path().join("opening-finish"), "")?;
    wait_idle(&daemon, &id).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(snapshot.sessions[0].approvals.is_empty());
    assert!(!snapshot.sessions[0].recovery_required);
    assert_eq!(std::fs::read_to_string(&path)?, "original");
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.text.contains("PLANK_EDITOR_STREAM"))
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn local_editor_opening_streams_wait_for_complete_text_before_publishing_the_form()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let initial = "\u{0}🦀\\\r\n".repeat(20_000);
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, &initial)?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = local_fixture_session(
        &daemon,
        directory.path(),
        vec!["--editor-open-stream".into()],
    )?;
    daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.path().join("opening-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(snapshot.sessions[0].approvals.is_empty());
    assert_eq!(snapshot.sessions[0].state, RunState::Running);
    std::fs::write(directory.path().join("opening-finish"), "")?;
    let editor = wait_question(&daemon, &id).await?;
    assert!(
        read_editor_document(&daemon, &id, &editor).await? == initial,
        "Opening must preserve the entire document"
    );
    assert_eq!(editor.tool, "editor.txt");
    daemon.execute(Command::EditDocument {
        session_id: id.clone(),
        approval_id: editor.id,
        requirement: editor.requirement,
        text: Some("saved 🦀\n".into()),
    })?;
    wait_idle(&daemon, &id).await?;
    assert_eq!(std::fs::read_to_string(&path)?, "saved 🦀\n");
    assert!(
        !daemon
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .any(|entry| entry.text.contains("PLANK_EDITOR_STREAM"))
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn local_editor_streams_preserve_acknowledgments_and_process_output_while_stdin_stalls()
-> Result<()> {
    for stalled in [false, true] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("editor.txt");
        std::fs::write(&path, "initial\n")?;
        let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
        let mut args = vec!["--editor-stream".into()];
        if stalled {
            args.push("--pause-editor-stream".into());
        }
        let id = local_fixture_session(&daemon, directory.path(), args)?;
        daemon.execute(prompt(&id, "open", "/open editor.txt"))?;
        let editor = wait_question(&daemon, &id).await?;
        let text = "\u{0}🦀\\\n".repeat(30_000);
        daemon.execute(Command::EditDocument {
            session_id: id.clone(),
            approval_id: editor.id,
            requirement: editor.requirement,
            text: Some(text.clone()),
        })?;
        let marker = directory.path().join(if stalled {
            "stream-started"
        } else {
            "stream-received"
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        assert_eq!(std::fs::read_to_string(&path)?, "initial\n");
        let current = daemon.snapshot(Some(&id), 0)?.sessions[0].clone();
        assert_eq!(current.state, RunState::Running);
        assert_eq!(
            current.approvals.len(),
            1,
            "Sending bytes must not acknowledge Save"
        );
        if stalled {
            std::fs::write(directory.path().join("close-editor"), "")?;
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if daemon.snapshot(Some(&id), 0).unwrap().sessions[0]
                        .approvals
                        .is_empty()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            assert_eq!(
                daemon.snapshot(Some(&id), 0)?.sessions[0].state,
                RunState::Running
            );
            daemon.execute(Command::Stop {
                session_id: id.clone(),
            })?;
            assert_eq!(std::fs::read_to_string(&path)?, "initial\n");
        } else {
            std::fs::write(directory.path().join("stream-ack"), "")?;
            wait_idle(&daemon, &id).await?;
            assert_eq!(std::fs::read_to_string(&path)?, text);
            daemon.execute(Command::Stop {
                session_id: id.clone(),
            })?;
        }
    }
    Ok(())
}

#[cfg(feature = "test-engine")]
#[tokio::test]
async fn document_edits_require_current_revision_and_explicit_save_or_cancel() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("editor.txt");
    std::fs::write(&path, "initial\n")?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Editor".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Plank".into(),
            backend: Backend::Local {
                executable: env!("CARGO_BIN_EXE_nplank-test-engine").into(),
                args: vec![],
            },
        })?
        .id;
    for (request, text) in [
        ("cancel", None),
        ("save", Some("explicit\n🦀\n".to_owned())),
    ] {
        daemon.execute(prompt(&id, request, "/open editor.txt"))?;
        let editor = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot = daemon.snapshot(Some(&id), 0)?;
                if let Some(editor) = snapshot.sessions[0].approvals.first() {
                    break Ok::<_, anyhow::Error>(editor.clone());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await??;
        assert_eq!(std::fs::read_to_string(&path)?, "initial\n");
        let decision = |requirement, text| Command::EditDocument {
            session_id: id.clone(),
            approval_id: editor.id.clone(),
            requirement,
            text,
        };
        assert!(
            daemon
                .execute(decision(
                    serde_json::json!({"revision": "old"}),
                    Some("wrong".into())
                ))
                .is_err()
        );
        daemon.execute(decision(editor.requirement.clone(), text.clone()))?;
        wait_idle(&daemon, &id).await?;
        assert_eq!(
            std::fs::read_to_string(&path)?,
            text.as_deref().unwrap_or("initial\n")
        );
        assert!(
            daemon
                .execute(decision(editor.requirement, Some("late".into())))
                .is_err()
        );
    }
    Ok(())
}
