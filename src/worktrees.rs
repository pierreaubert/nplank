//! Bounded Git operations for discovering and managing linked checkouts.

// Rust guideline compliant 2026-02-21
use crate::model::{Worktree, WorktreeChanges};
use anyhow::{Context, Result, bail, ensure};
use std::{
    io::Read,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

// Local Git operations must not indefinitely occupy a daemon command worker.
const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_LIMIT: u64 = 4 * 1024 * 1024;

fn git(directory: &str, args: &[&str]) -> Result<String> {
    let mut output = tempfile::tempfile()?;
    let mut errors = tempfile::tempfile()?;
    let mut child = std::process::Command::new("git")
        .args(["--no-optional-locks", "-C", directory])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(errors.try_clone()?)
        .spawn()
        .context("Cannot launch Git")?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if output.metadata()?.len() > OUTPUT_LIMIT || errors.metadata()?.len() > OUTPUT_LIMIT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Git output exceeds the limit; inspect this checkout in a terminal");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("Git operation timed out; refresh worktrees before retrying");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    use std::io::{Seek, SeekFrom};
    output.seek(SeekFrom::Start(0))?;
    errors.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    if status.success() {
        output.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes)?;
    } else {
        errors.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes)?;
        bail!("Git failed: {}", String::from_utf8_lossy(&bytes).trim());
    }
    ensure!(
        bytes.len() as u64 <= OUTPUT_LIMIT,
        "Git output exceeds the limit"
    );
    String::from_utf8(bytes).context("Git paths must be valid UTF-8")
}

/// Discovers all existing linked checkouts of the project containing `directory`.
pub(crate) fn discover(workspace_id: &str, directory: &str) -> Result<Vec<Worktree>> {
    let common = git(
        directory,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = std::fs::canonicalize(common.strip_suffix('\n').unwrap_or(&common))?
        .to_string_lossy()
        .into_owned();
    let listing = git(directory, &["worktree", "list", "--porcelain", "-z"])?;
    let mut trees = parse_listing(workspace_id, &common, &listing);
    let main = trees
        .first()
        .context("Git returned no worktrees")?
        .directory
        .clone();
    let project_name = Path::new(&main)
        .file_name()
        .context("Repository has no name")?
        .to_string_lossy()
        .into_owned();
    for tree in &mut trees {
        tree.project_name.clone_from(&project_name);
        // Missing/prunable checkouts are not offered as usable execution roots.
        if let Ok(path) = std::fs::canonicalize(&tree.directory) {
            tree.directory = path.to_string_lossy().into_owned();
        }
    }
    trees.retain(|tree| Path::new(&tree.directory).is_dir());
    Ok(trees)
}

fn parse_listing(workspace_id: &str, common: &str, listing: &str) -> Vec<Worktree> {
    let mut trees = Vec::new();
    let mut current: Option<Worktree> = None;
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(tree) = current.take() {
                trees.push(tree);
            }
            current = Some(Worktree {
                id: uuid::Uuid::new_v4().to_string(),
                workspace_id: workspace_id.into(),
                project_id: common.into(),
                project_name: String::new(),
                directory: path.into(),
                branch: None,
                main: trees.is_empty(),
                locked: false,
                removed: false,
            });
        } else if let Some(tree) = &mut current {
            if let Some(branch) = field.strip_prefix("branch refs/heads/") {
                tree.branch = Some(branch.into());
            }
            if field == "locked" || field.starts_with("locked ") {
                tree.locked = true;
            }
        }
    }
    if let Some(tree) = current {
        trees.push(tree);
    }
    trees
}

/// Rejects execution if the path now belongs to a different checkout or project.
pub(crate) fn verify(tree: &Worktree) -> Result<()> {
    ensure!(!tree.removed, "This checkout has been removed");
    let common = git(
        &tree.directory,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let root = git(&tree.directory, &["rev-parse", "--show-toplevel"])?;
    ensure!(
        std::fs::canonicalize(common.strip_suffix('\n').unwrap_or(&common))?
            == Path::new(&tree.project_id),
        "Checkout project changed; refresh before starting an agent"
    );
    ensure!(
        std::fs::canonicalize(root.strip_suffix('\n').unwrap_or(&root))?
            == Path::new(&tree.directory),
        "Checkout root changed; refresh before starting an agent"
    );
    Ok(())
}

/// Reads file status and diffs without running external diff drivers or text converters.
pub(crate) fn changes(tree: &Worktree) -> Result<WorktreeChanges> {
    verify(tree)?;
    let status = git(
        &tree.directory,
        &["status", "--short", "--untracked-files=normal"],
    )?;
    let staged = git(
        &tree.directory,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "--",
        ],
    )?;
    let unstaged = git(
        &tree.directory,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--color=never",
            "--",
        ],
    )?;
    Ok(WorktreeChanges {
        status,
        staged,
        unstaged,
    })
}

/// Creates a new branch and linked checkout without modifying the source checkout.
pub(crate) fn create(
    source: &Worktree,
    directory: &str,
    branch: &str,
    base: &str,
) -> Result<Vec<Worktree>> {
    verify(source)?;
    ensure!(
        Path::new(directory).is_absolute(),
        "Worktree directory must be absolute"
    );
    ensure!(
        !Path::new(directory).exists(),
        "Worktree destination already exists"
    );
    ensure!(
        !branch.starts_with('-') && !branch.is_empty(),
        "A new branch name is required"
    );
    git(&source.directory, &["check-ref-format", "--branch", branch])?;
    let revision = git(
        &source.directory,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    git(
        &source.directory,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            "--",
            directory,
            revision.trim(),
        ],
    )?;
    discover(&source.workspace_id, &source.directory)
}

/// Removes a clean, unlocked linked checkout; the branch and commits remain intact.
pub(crate) fn remove(tree: &Worktree) -> Result<()> {
    ensure!(!tree.main, "The main checkout cannot be removed");
    ensure!(
        !tree.locked,
        "Unlock the worktree in Git before removing it"
    );
    // Revalidate its identity and state in case Git changed since discovery.
    let current = discover(&tree.workspace_id, &tree.directory)?;
    let actual = current
        .iter()
        .find(|t| t.directory == tree.directory)
        .context("Worktree is no longer registered")?;
    ensure!(
        actual.project_id == tree.project_id && !actual.main && !actual.locked,
        "Worktree identity or lock changed; refresh first"
    );
    // Git itself refuses tracked changes and untracked files; never pass --force.
    let other = current
        .iter()
        .find(|t| t.directory != tree.directory)
        .context("No other checkout is available")?;
    git(
        &other.directory,
        &["worktree", "remove", "--", &tree.directory],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_preserves_paths_and_detached_and_locked_states() {
        let trees = parse_listing(
            "ws",
            "/repo/.git",
            "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0worktree /other with\nnewline\0HEAD abc\0detached\0locked maintenance\0\0",
        );
        assert_eq!(trees.len(), 2);
        assert!(trees[0].main);
        assert_eq!(trees[0].branch.as_deref(), Some("main"));
        assert_eq!(trees[1].directory, "/other with\nnewline");
        assert!(trees[1].locked);
        assert!(!trees[1].main);
        assert_eq!(trees[1].branch, None);
    }

    #[test]
    fn linked_checkouts_share_identity_and_dirty_removal_is_refused() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("project");
        std::fs::create_dir(&root)?;
        let root = root.to_str().unwrap();
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
        )?;
        let main = discover("ws", root)?.remove(0);
        let path = temp.path().join("feature checkout");
        let trees = create(&main, path.to_str().unwrap(), "feature/ui", "main")?;
        assert_eq!(trees.len(), 2);
        assert!(trees.iter().all(|tree| tree.project_id == main.project_id));
        let linked = trees.into_iter().find(|tree| !tree.main).unwrap();
        assert_eq!(linked.branch.as_deref(), Some("feature/ui"));
        std::fs::write(path.join("untracked.txt"), "keep this")?;
        assert!(remove(&linked).is_err());
        assert!(path.join("untracked.txt").exists());
        std::fs::remove_file(path.join("untracked.txt"))?;
        remove(&linked)?;
        assert!(!path.exists());
        assert!(git(root, &["rev-parse", "refs/heads/feature/ui"]).is_ok());
        Ok(())
    }
}
