//! Provider turns, durable resumption, approval revisions, and archive lifecycle.

// Rust guideline compliant 2026-02-21
#![cfg(feature = "test-engine")]
use anyhow::{Result, bail};
use nplank::{
    daemon::Daemon,
    model::{AgentProvider, Backend, Command, RunState},
    store::Store,
};
use std::{sync::Arc, time::Duration};

fn create(
    daemon: &Arc<Daemon>,
    directory: &std::path::Path,
    provider: AgentProvider,
) -> Result<String> {
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Project".into(),
            directory: directory.to_string_lossy().into(),
        })?
        .id;
    Ok(daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: provider.label().into(),
            backend: Backend::Agent {
                provider,
                executable: env!("CARGO_BIN_EXE_nplank-test-agent").into(),
                args: vec![
                    "--fixture-provider".into(),
                    match provider {
                        AgentProvider::MuseCode => "muse",
                        AgentProvider::Codex => "codex",
                        AgentProvider::ClaudeCode => "claude",
                    }
                    .into(),
                ],
                model: None,
            },
        })?
        .id)
}
fn prompt(daemon: &Arc<Daemon>, id: &str, text: &str) -> Result<()> {
    daemon.execute(Command::Prompt {
        session_id: id.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        text: text.into(),
    })?;
    Ok(())
}
async fn wait(daemon: &Daemon, id: &str, state: RunState) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let session = daemon
                .snapshot(Some(id), 0)?
                .sessions
                .into_iter()
                .find(|s| s.id == id)
                .expect("session");
            if session.state == state {
                return Ok(());
            }
            if session.state == RunState::Failed && state != RunState::Failed {
                bail!("{:?}", daemon.snapshot(Some(id), 0)?.entries);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}

#[tokio::test]
async fn codex_and_claude_resume_exact_saved_conversations_and_detect_failed_turns() -> Result<()> {
    for provider in [AgentProvider::Codex, AgentProvider::ClaudeCode] {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("state.sqlite");
        let daemon = Daemon::open(&path, "secret".into())?;
        let id = create(&daemon, directory.path(), provider)?;
        prompt(&daemon, &id, "first")?;
        wait(&daemon, &id, RunState::Idle).await?;
        prompt(&daemon, &id, "second")?;
        wait(&daemon, &id, RunState::Idle).await?;
        assert!(
            daemon
                .snapshot(Some(&id), 0)?
                .entries
                .iter()
                .any(|e| e.text.contains("resumed=true: second"))
        );
        prompt(&daemon, &id, "fail")?;
        wait(&daemon, &id, RunState::Failed).await?;
        assert!(prompt(&daemon, &id, "blocked").is_err());
        daemon.execute(Command::Resume {
            session_id: id.clone(),
        })?;
        prompt(&daemon, &id, "after failure")?;
        wait(&daemon, &id, RunState::Idle).await?;
        daemon.execute(Command::ArchiveSession {
            session_id: id.clone(),
            archived: true,
        })?;
        assert!(prompt(&daemon, &id, "archived").is_err());
        daemon.execute(Command::ArchiveSession {
            session_id: id.clone(),
            archived: false,
        })?;
        // A new daemon uses the saved identity, never the CLI's ambient latest session.
        let reopened = Daemon::open(&path, "secret".into())?;
        prompt(&reopened, &id, "after restart")?;
        wait(&reopened, &id, RunState::Idle).await?;
        assert!(
            reopened
                .snapshot(Some(&id), 0)?
                .entries
                .iter()
                .any(|e| e.text.contains("resumed=true: after restart"))
        );
        reopened.execute(Command::Reset {
            session_id: id.clone(),
        })?;
        prompt(&reopened, &id, "fresh")?;
        wait(&reopened, &id, RunState::Idle).await?;
        assert!(
            reopened
                .snapshot(Some(&id), 0)?
                .entries
                .iter()
                .any(|e| e.text.contains("resumed=false: fresh"))
        );
    }
    Ok(())
}

#[tokio::test]
async fn muse_streams_without_duplicate_final_text_and_requires_current_approval_revision()
-> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let id = create(&daemon, directory.path(), AgentProvider::MuseCode)?;
    prompt(&daemon, &id, "hello")?;
    wait(&daemon, &id, RunState::Idle).await?;
    let transcript: String = daemon
        .snapshot(Some(&id), 0)?
        .entries
        .into_iter()
        .filter(|e| e.role == "assistant")
        .map(|e| e.text)
        .collect();
    assert_eq!(transcript, "Muse: hello €");
    prompt(&daemon, &id, "approve")?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !daemon
                .snapshot(Some(&id), 0)?
                .sessions
                .iter()
                .find(|s| s.id == id)
                .expect("session")
                .approvals
                .is_empty()
            {
                return Ok::<(), anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    let decision = |revision: &str| Command::DecideApproval {
        session_id: id.clone(),
        approval_id: "approval-1".into(),
        choice_id: "allow".into(),
        requirement: serde_json::json!(revision),
    };
    assert!(daemon.execute(decision("outdated")).is_err());
    daemon.execute(decision("revision-1"))?;
    wait(&daemon, &id, RunState::Idle).await?;
    assert!(
        daemon
            .snapshot(Some(&id), 0)?
            .entries
            .iter()
            .any(|e| e.text.contains("approve: allow"))
    );
    assert!(
        daemon
            .snapshot(Some(&id), 0)?
            .sessions
            .iter()
            .find(|s| s.id == id)
            .expect("session")
            .approvals
            .is_empty()
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}

#[test]
fn session_order_swaps_atomically_and_workspace_renames_preserve_directories() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let store = Store::open(&directory.path().join("state.sqlite"))?;
    let workspace = store.create_workspace("Before", directory.path().to_str().expect("path"))?;
    let a = store.create_session(&workspace, "A", Backend::Demo)?;
    let b = store.create_session(&workspace, "B", Backend::Demo)?;
    store.reorder(&b, store.session(&a)?.position)?;
    assert_eq!(store.snapshot(None, 0)?.sessions[0].id, b);
    store.rename_workspace(&workspace, "After")?;
    assert_eq!(store.workspace(&workspace)?.name, "After");
    assert_eq!(
        store.workspace(&workspace)?.directory,
        directory.path().canonicalize()?.to_str().expect("path")
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an installed Muse CLI; uses its model-free echo provider"]
async fn installed_muse_echo_protocol() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let daemon = Daemon::open(&directory.path().join("state.sqlite"), "secret".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Muse smoke".into(),
            directory: directory.path().to_string_lossy().into(),
        })?
        .id;
    let id = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace,
            name: "Muse echo".into(),
            backend: Backend::Agent {
                provider: AgentProvider::MuseCode,
                executable: std::env::var("NPLANK_MUSE_EXECUTABLE")
                    .unwrap_or_else(|_| "muse".into()),
                args: vec![
                    "--provider".into(),
                    "echo".into(),
                    "--no-session-log".into(),
                ],
                model: None,
            },
        })?
        .id;
    prompt(&daemon, &id, "nplank smoke €")?;
    wait(&daemon, &id, RunState::Idle).await?;
    let snapshot = daemon.snapshot(Some(&id), 0)?;
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.role == "assistant" && entry.text.contains("nplank smoke €")),
        "{:?}",
        snapshot.entries
    );
    assert!(
        !snapshot
            .sessions
            .iter()
            .find(|s| s.id == id)
            .expect("session")
            .resumable
    );
    daemon.execute(Command::Stop { session_id: id })?;
    Ok(())
}
