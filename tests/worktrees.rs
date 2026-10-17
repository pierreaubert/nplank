//! Durable checkout isolation and legacy-session migration regressions.

// Rust guideline compliant 2026-02-21
use anyhow::Result;
use nplank::{
    daemon::Daemon,
    model::{Backend, Command, RunState},
    store::Store,
};
use std::{path::Path, process::Command as Process};

fn git(root: &Path, args: &[&str]) -> Result<()> {
    let result = Process::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(())
}

fn repository(root: &Path) -> Result<()> {
    std::fs::create_dir(root)?;
    git(root, &["init", "-b", "main"])?;
    git(
        root,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Initial",
        ],
    )
}

#[test]
fn discover_preserves_identity_and_session_directories_across_restart() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    let linked = tmp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    )?;
    let path = tmp.path().join("state.sqlite");
    let daemon = Daemon::open(&path, "test".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Development".into(),
            directory: main.to_string_lossy().into_owned(),
        })?
        .id;
    let legacy = daemon
        .execute(Command::CreateSession {
            workspace_id: workspace.clone(),
            name: "Existing".into(),
            backend: Backend::Demo,
        })?
        .id;
    assert_eq!(
        daemon.snapshot(None, 0)?.worktrees.len(),
        2,
        "Creating a Git workspace discovers its linked checkouts automatically"
    );
    daemon.execute(Command::DiscoverWorktrees {
        workspace_id: workspace.clone(),
        directory: main.to_string_lossy().into_owned(),
    })?;
    let before = daemon.snapshot(None, 0)?;
    assert_eq!(before.worktrees.len(), 2);
    let main_id = before
        .worktrees
        .iter()
        .find(|tree| tree.main)
        .unwrap()
        .id
        .clone();
    assert_eq!(
        before
            .sessions
            .iter()
            .find(|s| s.id == legacy)
            .unwrap()
            .worktree_id
            .as_ref(),
        Some(&main_id)
    );
    let tree = before.worktrees.iter().find(|tree| !tree.main).unwrap();
    let session = daemon
        .execute(Command::CreateWorktreeSession {
            worktree_id: tree.id.clone(),
            name: "Feature".into(),
            backend: Backend::Demo,
        })?
        .id;
    daemon.execute(Command::DiscoverWorktrees {
        workspace_id: workspace,
        directory: linked.to_string_lossy().into_owned(),
    })?;
    assert_eq!(daemon.snapshot(None, 0)?.worktrees, before.worktrees);
    let store = Store::open(&path)?;
    assert_eq!(
        store.session_directory(&store.session(&session)?)?,
        std::fs::canonicalize(&linked)?.to_string_lossy()
    );
    assert_eq!(
        store.session_directory(&store.session(&legacy)?)?,
        std::fs::canonicalize(&main)?.to_string_lossy()
    );
    assert!(
        daemon
            .execute(Command::RemoveWorktree {
                worktree_id: tree.id.clone()
            })
            .is_err()
    );
    store.set_state(&session, RunState::Running, false)?;
    assert!(store.archive(&session, true).is_err());
    store.set_state(&session, RunState::Idle, false)?;
    store.archive(&session, true)?;
    daemon.execute(Command::RemoveWorktree {
        worktree_id: tree.id.clone(),
    })?;
    assert!(!linked.exists());
    assert!(store.session(&session)?.archived);
    assert!(store.session_directory(&store.session(&session)?).is_err());
    assert!(store.archive(&session, false).is_err());
    assert_eq!(
        store.session_directory(&store.session(&legacy)?)?,
        std::fs::canonicalize(&main)?.to_string_lossy()
    );
    Ok(())
}

#[test]
fn create_checkout_does_not_switch_source_branch_or_delete_branch_on_removal() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    let daemon = Daemon::open(&tmp.path().join("state.sqlite"), "test".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Work".into(),
            directory: main.to_string_lossy().into_owned(),
        })?
        .id;
    daemon.execute(Command::DiscoverWorktrees {
        workspace_id: workspace,
        directory: main.to_string_lossy().into_owned(),
    })?;
    let source_id = daemon.snapshot(None, 0)?.worktrees[0].id.clone();
    let linked = tmp.path().join("new checkout");
    let tree = daemon
        .execute(Command::CreateWorktree {
            source_id,
            directory: linked.to_string_lossy().into_owned(),
            branch: "feature/ui".into(),
            base: "main".into(),
        })?
        .id;
    let branch = Process::new("git")
        .arg("-C")
        .arg(&main)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()?;
    assert!(branch.status.success());
    assert_eq!(String::from_utf8(branch.stdout)?.trim(), "refs/heads/main");
    std::fs::write(linked.join("untracked"), "do not delete")?;
    assert!(
        daemon
            .execute(Command::RemoveWorktree {
                worktree_id: tree.clone()
            })
            .is_err()
    );
    assert!(linked.join("untracked").exists());
    std::fs::remove_file(linked.join("untracked"))?;
    daemon.execute(Command::RemoveWorktree { worktree_id: tree })?;
    git(&main, &["show-ref", "--verify", "refs/heads/feature/ui"])?;
    Ok(())
}

#[test]
fn removal_checks_sessions_in_other_workspaces_sharing_the_checkout() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    let linked = tmp.path().join("linked");
    git(
        &main,
        &["worktree", "add", "-b", "feature", linked.to_str().unwrap()],
    )?;
    let daemon = Daemon::open(&tmp.path().join("state.sqlite"), "test".into())?;
    let mut tree_ids = Vec::new();
    for name in ["First", "Second"] {
        let workspace = daemon
            .execute(Command::CreateWorkspace {
                name: name.into(),
                directory: main.to_string_lossy().into_owned(),
            })?
            .id;
        daemon.execute(Command::DiscoverWorktrees {
            workspace_id: workspace.clone(),
            directory: main.to_string_lossy().into_owned(),
        })?;
        tree_ids.push(
            daemon
                .snapshot(None, 0)?
                .worktrees
                .into_iter()
                .find(|t| t.workspace_id == workspace && !t.main)
                .unwrap()
                .id,
        );
    }
    let session = daemon
        .execute(Command::CreateWorktreeSession {
            worktree_id: tree_ids[1].clone(),
            name: "Do not remove".into(),
            backend: Backend::Demo,
        })?
        .id;
    assert!(
        daemon
            .execute(Command::RemoveWorktree {
                worktree_id: tree_ids[0].clone()
            })
            .is_err()
    );
    assert!(linked.exists());
    daemon.execute(Command::ArchiveSession {
        session_id: session,
        archived: true,
    })?;
    daemon.execute(Command::RemoveWorktree {
        worktree_id: tree_ids[0].clone(),
    })?;
    assert!(
        daemon
            .snapshot(None, 0)?
            .worktrees
            .iter()
            .filter(|t| tree_ids.contains(&t.id))
            .all(|t| t.removed)
    );
    Ok(())
}

#[test]
fn inspection_separates_staged_unstaged_and_untracked_without_changing_files() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    std::fs::write(main.join("file.txt"), "base\n")?;
    git(&main, &["add", "file.txt"])?;
    git(
        &main,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "Track file",
        ],
    )?;
    std::fs::write(main.join("file.txt"), "staged\n")?;
    git(&main, &["add", "file.txt"])?;
    std::fs::write(main.join("file.txt"), "unstaged\n")?;
    std::fs::write(main.join("new file.txt"), "untracked content")?;
    let daemon = Daemon::open(&tmp.path().join("state.sqlite"), "test".into())?;
    let workspace = daemon
        .execute(Command::CreateWorkspace {
            name: "Review".into(),
            directory: main.to_string_lossy().into(),
        })?
        .id;
    daemon.execute(Command::DiscoverWorktrees {
        workspace_id: workspace,
        directory: main.to_string_lossy().into(),
    })?;
    let tree = daemon.snapshot(None, 0)?.worktrees.remove(0);
    let changes = daemon.worktree_changes(&tree.id)?;
    assert!(changes.status.contains("MM file.txt"));
    assert!(changes.status.contains("?? \"new file.txt\""));
    assert!(changes.staged.contains("-base\n+staged"));
    assert!(changes.unstaged.contains("-staged\n+unstaged"));
    assert!(!changes.staged.contains("untracked content"));
    assert_eq!(
        std::fs::read_to_string(main.join("file.txt"))?,
        "unstaged\n"
    );
    assert!(daemon.worktree_changes("unknown").is_err());
    Ok(())
}

#[tokio::test]
async fn checkout_inspection_requires_authentication_and_does_not_cache_diffs() -> Result<()> {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    let daemon = Daemon::open(&tmp.path().join("state.sqlite"), "secret".into())?;
    daemon.execute(Command::CreateWorkspace {
        name: "Review".into(),
        directory: main.to_string_lossy().into(),
    })?;
    let tree = daemon.snapshot(None, 0)?.worktrees.remove(0);
    let route = format!("/v1/worktree-changes?id={}", tree.id);
    let unauthorized = nplank::daemon::router(daemon.clone())
        .oneshot(Request::builder().uri(&route).body(Body::empty())?)
        .await?;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let response = nplank::daemon::router(daemon)
        .oneshot(
            Request::builder()
                .uri(&route)
                .header("authorization", "Bearer secret")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let changes: nplank::model::WorktreeChanges =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024).await?)?;
    assert!(changes.status.is_empty());
    Ok(())
}

#[test]
fn startup_discovers_existing_git_workspaces_without_requiring_a_manager_action() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let main = tmp.path().join("main");
    repository(&main)?;
    let path = tmp.path().join("state.sqlite");
    let store = Store::open(&path)?;
    let workspace = store.create_workspace("Existing", main.to_str().unwrap())?;
    let session = store.create_session(&workspace, "Existing agent", Backend::Demo)?;
    let daemon = Daemon::open(&path, "test".into())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let snapshot = daemon.snapshot(None, 0)?;
        if let Some(tree) = snapshot.worktrees.first() {
            assert_eq!(
                snapshot
                    .sessions
                    .iter()
                    .find(|s| s.id == session)
                    .unwrap()
                    .worktree_id
                    .as_ref(),
                Some(&tree.id)
            );
            break;
        }
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "Startup discovery timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Ok(())
}
