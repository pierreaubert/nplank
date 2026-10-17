//! Bounded host probes cached away from HTTP requests and UI rendering.

// Rust guideline compliant 2026-02-21
use crate::{daemon::Daemon, model::SessionContext};
use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    sync::{Arc, Weak},
    time::Duration,
};
use tokio::io::AsyncReadExt;

#[derive(Debug)]
pub(crate) struct ProcessLease {
    daemon: Weak<Daemon>,
    id: String,
    generation: u64,
    pid: u32,
}
impl ProcessLease {
    pub fn new(daemon: &Arc<Daemon>, session: &crate::model::Session, pid: Option<u32>) -> Self {
        let pid = pid.unwrap_or(0);
        daemon
            .processes
            .lock()
            .expect("processes poisoned")
            .insert(session.id.clone(), (session.generation, pid));
        Self {
            daemon: Arc::downgrade(daemon),
            id: session.id.clone(),
            generation: session.generation,
            pid,
        }
    }
}
impl Drop for ProcessLease {
    fn drop(&mut self) {
        if let Some(daemon) = self.daemon.upgrade() {
            let mut processes = daemon.processes.lock().expect("processes poisoned");
            if processes.get(&self.id) == Some(&(self.generation, self.pid)) {
                processes.remove(&self.id);
            }
        }
    }
}

pub(crate) fn start(daemon: &Arc<Daemon>) {
    let weak = Arc::downgrade(daemon);
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        runtime.block_on(async move {
            loop {
                let Some(daemon) = weak.upgrade() else {
                    break;
                };
                let snapshot = daemon
                    .store
                    .lock()
                    .expect("store poisoned")
                    .snapshot(None, 0);
                let processes = daemon.processes.lock().expect("processes poisoned").clone();
                drop(daemon);
                if let Ok(snapshot) = snapshot {
                    let mut git = HashMap::new();
                    let directories: HashSet<&str> = snapshot
                        .workspaces
                        .iter()
                        .map(|w| w.directory.as_str())
                        .chain(snapshot.worktrees.iter().map(|t| t.directory.as_str()))
                        .collect();
                    for directory in directories {
                        let result = probe(
                            "git",
                            &[
                                "--no-optional-locks",
                                "-C",
                                directory,
                                "status",
                                "--porcelain=v1",
                                "--branch",
                                "--untracked-files=normal",
                            ],
                        )
                        .await;
                        git.insert(directory, result.map(|text| parse_git(&text)));
                    }
                    let parents = if cfg!(unix) {
                        probe("ps", &["-axo", "pid=,ppid="]).await
                    } else {
                        None
                    };
                    let listeners = if cfg!(unix) {
                        probe("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]).await
                    } else {
                        None
                    };
                    let mut contexts = HashMap::new();
                    for session in &snapshot.sessions {
                        let mut context = SessionContext::default();
                        let directory = if let Some(id) = &session.worktree_id {
                            snapshot
                                .worktrees
                                .iter()
                                .find(|tree| &tree.id == id)
                                .map(|tree| tree.directory.as_str())
                        } else {
                            snapshot
                                .workspaces
                                .iter()
                                .find(|workspace| workspace.id == session.workspace_id)
                                .map(|workspace| workspace.directory.as_str())
                        };
                        if let Some(Some((branch, dirty))) =
                            directory.and_then(|directory| git.get(directory))
                        {
                            context.branch = branch.clone();
                            context.dirty = *dirty;
                        }
                        if let Some((generation, pid)) = processes.get(&session.id)
                            && *generation == session.generation
                            && *pid != 0
                            && let (Some(parents), Some(listeners)) = (&parents, &listeners)
                        {
                            context.ports =
                                Some(parse_ports(listeners, &descendants(*pid, parents)));
                        }
                        contexts.insert(session.id.clone(), (session.generation, context));
                    }
                    if let Some(daemon) = weak.upgrade() {
                        *daemon.contexts.lock().expect("contexts poisoned") = contexts;
                    } else {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    });
}

async fn probe(command: &str, args: &[&str]) -> Option<String> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        let status = child.wait().await.ok()?;
        // lsof returns 1 when no listening sockets exist.
        if (!status.success()
            && !(command == "lsof" && status.code() == Some(1) && bytes.is_empty()))
            || bytes.len() > 1024 * 1024
        {
            return None;
        }
        String::from_utf8(bytes).ok()
    })
    .await
    .ok()
    .flatten()
}

fn parse_git(text: &str) -> (Option<String>, bool) {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("").trim_start_matches("## ");
    let header = header
        .strip_prefix("No commits yet on ")
        .or_else(|| header.strip_prefix("Initial commit on "))
        .unwrap_or(header);
    (
        Some(header.split("...").next().unwrap_or(header).to_owned()).filter(|s| !s.is_empty()),
        lines.next().is_some(),
    )
}
fn descendants(root: u32, parents: &str) -> HashSet<u32> {
    let pairs: Vec<(u32, u32)> = parents
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
        })
        .collect();
    let mut owned = HashSet::from([root]);
    for _ in 0..64 {
        let count = owned.len();
        for (pid, parent) in &pairs {
            if owned.contains(parent) {
                owned.insert(*pid);
            }
        }
        if owned.len() == count {
            break;
        }
    }
    owned
}
fn parse_ports(listeners: &str, owned: &HashSet<u32>) -> Vec<u16> {
    let mut pid = 0;
    let mut ports = Vec::new();
    for line in listeners.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().unwrap_or(0);
        }
        if owned.contains(&pid)
            && let Some(name) = line.strip_prefix('n')
            && let Some((_, port)) = name.rsplit_once(':')
            && let Ok(port) = port.parse()
        {
            ports.push(port);
        }
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_scopes_ports_to_owned_processes_and_detects_git_changes() {
        let owned = descendants(10, "10 1\n11 10\n12 11\n99 1");
        assert_eq!(
            parse_ports(
                "p11\nn127.0.0.1:3000\nn[::1]:3000\np99\nn*:8080\np12\nn*:4789",
                &owned
            ),
            vec![3000, 4789]
        );
        assert_eq!(
            parse_git("## feature/ui...origin/feature/ui\n M src/ui.rs\n"),
            (Some("feature/ui".into()), true)
        );
        assert_eq!(
            parse_git("## No commits yet on main\n"),
            (Some("main".into()), false)
        );
    }
}
