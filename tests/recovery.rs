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
    }
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
fn remote_snapshot_replay_is_deduplicated_and_pages_are_bounded() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace =
        store.create_workspace("Project", directory.path().to_str().expect("temp path"))?;
    let id = store.create_session(&workspace, "Agent", Backend::Demo)?;
    store.append_remote(&id, 1, "assistant", "once")?;
    store.append_remote(&id, 1, "assistant", "once")?;
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
