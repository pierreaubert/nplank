//! Responsive navigation policy and user-action status, independent of rendering.

// Rust guideline compliant 2026-02-21
use crate::model::{Backend, RunState, Session, Snapshot, Worktree};
use serde::{Deserialize, Serialize};

/// Resolves display grouping without changing a legacy session's execution directory.
pub(crate) fn session_group_worktree<'a>(
    snapshot: &'a Snapshot,
    session: &Session,
) -> Option<&'a Worktree> {
    if matches!(session.backend, Backend::Remote { .. }) {
        return None;
    }
    if let Some(id) = &session.worktree_id {
        // Never reinterpret a missing explicit association as another checkout.
        return snapshot
            .worktrees
            .iter()
            .find(|tree| &tree.id == id && tree.workspace_id == session.workspace_id);
    }
    let workspace = snapshot
        .workspaces
        .iter()
        .find(|w| w.id == session.workspace_id)?;
    snapshot
        .worktrees
        .iter()
        .filter(|tree| {
            tree.workspace_id == session.workspace_id
                && !tree.removed
                && std::path::Path::new(&workspace.directory).starts_with(&tree.directory)
        })
        .max_by_key(|tree| std::path::Path::new(&tree.directory).components().count())
}

/// Saved navigation preference; automatic mode reserves readable conversation space.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NavigationMode {
    #[default]
    Auto,
    Pinned,
    Hidden,
}

impl NavigationMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Pinned => "Pinned",
            Self::Hidden => "Hidden",
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::Auto => Self::Pinned,
            Self::Pinned => Self::Hidden,
            Self::Hidden => Self::Auto,
        }
    }

    pub(crate) fn docked(self, width: f32, sidebar: f32) -> bool {
        // Compact laptops and tablets default to the rail even if text could fit.
        // Wide windows reserve 1200 logical pixels for conversation and inspector.
        // Below 700 pixels even pinned navigation must overlay, never crush text.
        width >= 700.0
            && match self {
                Self::Auto => width >= 1600.0 && width >= sidebar + 1200.0,
                Self::Pinned => true,
                Self::Hidden => false,
            }
    }
}

/// User intervention severity, independent of whether an agent is executing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AttentionLevel {
    #[default]
    Clear,
    Pending,
    Unknown,
    Required,
}

impl AttentionLevel {
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            Self::Clear => "✓",
            Self::Pending => "?",
            Self::Unknown => "◇",
            Self::Required => "!",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Clear => "No action needed",
            Self::Pending => "Choice pending",
            Self::Unknown => "Disconnected · status unknown",
            Self::Required => "Attention required",
        }
    }
}

/// Returns intervention severity for one session, including blocked recovery.
pub(crate) fn session_attention(session: &Session) -> AttentionLevel {
    if session.recovery_required
        || matches!(session.state, RunState::Failed | RunState::Interrupted)
    {
        AttentionLevel::Required
    } else if session.state == RunState::Disconnected {
        AttentionLevel::Unknown
    } else if !session.approvals.is_empty() || (session.attention && session.completed_at.is_none())
    {
        AttentionLevel::Pending
    } else {
        AttentionLevel::Clear
    }
}

/// Aggregates all nonarchived sessions, including those hidden by navigation filters.
pub(crate) fn attention_summary<'a>(
    sessions: impl IntoIterator<Item = &'a Session>,
    connected: bool,
) -> (AttentionLevel, usize) {
    if !connected {
        return (AttentionLevel::Unknown, 0);
    }
    sessions.into_iter().filter(|s| !s.archived).fold(
        (AttentionLevel::Clear, 0),
        |(level, count), session| {
            let next = session_attention(session);
            (
                level.max(next),
                count + usize::from(next != AttentionLevel::Clear),
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subdirectory_grouping_preserves_execution_and_explicit_identities() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let nested = tmp.path().join("project").join("nested");
        let directory = nested.join("src");
        std::fs::create_dir_all(&directory)?;
        let nested = nested.canonicalize()?;
        let store = crate::store::Store::open(&tmp.path().join("state.sqlite"))?;
        let workspace = store.create_workspace("Nested", directory.to_str().unwrap())?;
        let id = store.create_session(&workspace, "Existing", Backend::Demo)?;
        let mut session = store.session(&id)?;
        let tree = Worktree {
            id: "nested".into(),
            workspace_id: workspace,
            project_id: "nested/.git".into(),
            project_name: "Nested".into(),
            directory: nested.to_str().unwrap().into(),
            branch: Some("main".into()),
            main: true,
            locked: false,
            removed: false,
        };
        let mut parent = tree.clone();
        parent.id = "parent".into();
        parent.directory = nested.parent().unwrap().to_str().unwrap().into();
        store.attach_worktrees(vec![parent, tree])?;
        let mut snapshot = store.snapshot(None, 0)?;
        assert_eq!(
            session_group_worktree(&snapshot, &session).unwrap().id,
            "nested"
        );
        assert!(store.session(&id)?.worktree_id.is_none());
        assert_eq!(
            store.session_directory(&session)?,
            directory.canonicalize()?.to_str().unwrap()
        );

        session.worktree_id = Some("missing".into());
        assert!(session_group_worktree(&snapshot, &session).is_none());
        session.worktree_id = Some("parent".into());
        assert_eq!(
            session_group_worktree(&snapshot, &session).unwrap().id,
            "parent"
        );
        session.worktree_id = None;
        snapshot
            .worktrees
            .iter_mut()
            .for_each(|tree| tree.removed = true);
        assert!(session_group_worktree(&snapshot, &session).is_none());
        snapshot
            .worktrees
            .iter_mut()
            .for_each(|tree| tree.removed = false);
        session.backend = Backend::Remote {
            url: "ws://localhost:1".into(),
            token_env: "TEST_TOKEN".into(),
        };
        assert!(session_group_worktree(&snapshot, &session).is_none());
        session.backend = Backend::Demo;
        snapshot.workspaces[0].directory = format!("{}-other", nested.parent().unwrap().display());
        assert!(session_group_worktree(&snapshot, &session).is_none());
        Ok(())
    }

    #[test]
    fn automatic_navigation_preserves_conversation_width() {
        assert!(!NavigationMode::Auto.docked(1024.0, 270.0));
        assert!(!NavigationMode::Auto.docked(1130.0, 270.0));
        assert!(!NavigationMode::Auto.docked(1440.0, 270.0));
        assert!(!NavigationMode::Auto.docked(1470.0, 270.0));
        assert!(NavigationMode::Auto.docked(1600.0, 270.0));
        assert!(!NavigationMode::Auto.docked(1600.0, 520.0));
        assert!(NavigationMode::Auto.docked(1920.0, 520.0));
        assert!(!NavigationMode::Auto.docked(1280.0, 520.0));
        assert!(NavigationMode::Pinned.docked(1024.0, 270.0));
        assert!(!NavigationMode::Pinned.docked(600.0, 270.0));
        assert!(!NavigationMode::Hidden.docked(1920.0, 270.0));
    }

    #[test]
    fn disconnected_client_never_claims_no_action_needed() {
        assert_eq!(attention_summary([], false), (AttentionLevel::Unknown, 0));
        assert_eq!(attention_summary([], true), (AttentionLevel::Clear, 0));
    }
}
