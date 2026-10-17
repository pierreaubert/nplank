//! Machine derivation for the sidebar's Machines section.
//!
//! The daemon has no machine entity: a session runs either on this
//! machine (local, demo, or third-party agent backends) or on a remote
//! Plank host (`Backend::Remote`). Machines are derived from the
//! snapshot so the grouping can never disagree with the sessions shown.

use crate::model::{Backend, Snapshot};

/// Stable id for the machine this client runs on.
pub const LOCAL_MACHINE: &str = "local";

/// One machine with the sessions running on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    /// `local`, or the remote host (`host` or `host:port`) as written in
    /// the backend URL.
    pub id: String,
    /// Display name: "This Mac" or the remote host.
    pub name: String,
    /// Session ids on this machine, in snapshot order.
    pub session_ids: Vec<String>,
}

/// Returns the machine id + display name for a session backend.
fn machine_of(backend: &Backend) -> (String, String) {
    match backend {
        Backend::Remote { url, .. } => {
            let host = url
                .split("://")
                .nth(1)
                .unwrap_or(url.as_str())
                .split(['/', '?', '#'])
                .next()
                .unwrap_or(url.as_str());
            let host = if host.is_empty() { url.as_str() } else { host };
            (host.to_owned(), host.to_owned())
        }
        Backend::Demo | Backend::Local { .. } | Backend::Agent { .. } => {
            (LOCAL_MACHINE.to_owned(), "This Mac".to_owned())
        }
    }
}

/// Groups non-archived sessions by machine: this machine first, then
/// remote hosts in first-seen order.
pub fn machines(snapshot: &Snapshot) -> Vec<Machine> {
    let mut result: Vec<Machine> = Vec::new();
    for session in snapshot.sessions.iter().filter(|s| !s.archived) {
        let (id, name) = machine_of(&session.backend);
        match result.iter_mut().find(|machine| machine.id == id) {
            Some(machine) => machine.session_ids.push(session.id.clone()),
            None => result.push(Machine {
                id,
                name,
                session_ids: vec![session.id.clone()],
            }),
        }
    }
    result.sort_by_key(|machine| machine.id != LOCAL_MACHINE);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RunState, Session, Workspace};

    fn session(id: &str, backend: Backend) -> Session {
        Session {
            worktree_id: None,
            progress: None,
            controls: None,
            attention: false,
            started_at: None,
            completed_at: None,
            summary: String::new(),
            id: id.into(),
            workspace_id: "w1".into(),
            name: id.into(),
            backend,
            state: RunState::Idle,
            recovery_required: false,
            generation: 0,
            archived: false,
            position: 0,
            agent_session: None,
            resumable: false,
            approvals: Vec::new(),
        }
    }

    fn snapshot(sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            workspaces: vec![Workspace {
                id: "w1".into(),
                name: "W".into(),
                directory: "/w".into(),
            }],
            sessions,
            ..Snapshot::default()
        }
    }

    #[test]
    fn local_sessions_group_under_this_mac() {
        let snap = snapshot(vec![
            session(
                "a",
                Backend::Local {
                    executable: "plank".into(),
                    args: vec![],
                },
            ),
            session("b", Backend::Demo),
        ]);
        let machines = machines(&snap);
        assert_eq!(machines.len(), 1);
        assert_eq!(machines[0].id, LOCAL_MACHINE);
        assert_eq!(machines[0].name, "This Mac");
        assert_eq!(machines[0].session_ids, ["a", "b"]);
    }

    #[test]
    fn remote_hosts_group_by_host_with_port() {
        let snap = snapshot(vec![
            session(
                "r1",
                Backend::Remote {
                    url: "wss://build.example.com:9000/plank?x=1".into(),
                    token_env: "T".into(),
                },
            ),
            session(
                "l1",
                Backend::Local {
                    executable: "plank".into(),
                    args: vec![],
                },
            ),
            session(
                "r2",
                Backend::Remote {
                    url: "wss://build.example.com:9000/other".into(),
                    token_env: "T".into(),
                },
            ),
        ]);
        let machines = machines(&snap);
        assert_eq!(machines.len(), 2);
        // This Mac first even though the remote session came first.
        assert_eq!(machines[0].id, LOCAL_MACHINE);
        assert_eq!(machines[1].id, "build.example.com:9000");
        assert_eq!(machines[1].session_ids, ["r1", "r2"]);
    }

    #[test]
    fn archived_sessions_have_no_machine() {
        let mut gone = session("gone", Backend::Demo);
        gone.archived = true;
        let snap = snapshot(vec![gone]);
        assert!(machines(&snap).is_empty());
    }
}
