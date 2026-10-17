//! Top-bar derivations: per-provider usage and the attention queue.
//!
//! Quota ("weekly left") is not exposed by any provider the daemon talks
//! to, so usage is derived from live session telemetry: sessions,
//! running count, generated tokens, and peak context fill per provider.
//! The attention queue reuses Fleet's attention-first ordering so the
//! "where next" answer always matches the Fleet view.

use crate::fleet::{AttentionReason, fleet_cards};
use crate::model::{Backend, Snapshot};

/// Aggregated live usage for one provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUsage {
    /// Display label (`Local Plank`, `Codex`, …).
    pub label: String,
    /// Live (non-archived) sessions on this provider.
    pub sessions: usize,
    /// Sessions currently executing.
    pub running: usize,
    /// Generated tokens summed over sessions reporting telemetry.
    pub generated: u64,
    /// Highest context fill `0..=100` across the provider's sessions.
    pub ctx_peak: Option<u8>,
}

fn provider_label(backend: &Backend) -> String {
    match backend {
        Backend::Demo => "Demo".to_owned(),
        Backend::Local { .. } => "Local Plank".to_owned(),
        Backend::Remote { .. } => "Remote Plank".to_owned(),
        Backend::Agent { provider, .. } => provider.label().to_owned(),
    }
}

/// Per-provider usage sorted by activity: running first, then tokens.
pub fn provider_usage(snapshot: &Snapshot) -> Vec<ProviderUsage> {
    let mut result: Vec<ProviderUsage> = Vec::new();
    for session in snapshot.sessions.iter().filter(|s| !s.archived) {
        let label = provider_label(&session.backend);
        let status = snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.status.as_ref());
        let index = match result.iter().position(|usage| usage.label == label) {
            Some(index) => index,
            None => {
                result.push(ProviderUsage {
                    label: label.clone(),
                    sessions: 0,
                    running: 0,
                    generated: 0,
                    ctx_peak: None,
                });
                result.len() - 1
            }
        };
        let entry = &mut result[index];
        entry.sessions += 1;
        if session.state.busy() {
            entry.running += 1;
        }
        if let Some(status) = status {
            entry.generated = entry.generated.saturating_add(status.generated);
            if let Some(percent) = status
                .ctx_used
                .saturating_mul(100)
                .checked_div(status.ctx_size)
                .map(|percent| percent.min(100) as u8)
            {
                entry.ctx_peak = Some(entry.ctx_peak.map_or(percent, |peak| peak.max(percent)));
            }
        }
    }
    result.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then(b.generated.cmp(&a.generated))
            .then(a.label.cmp(&b.label))
    });
    result
}

/// One session waiting for the user, in Fleet attention order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionItem {
    /// Session id to jump to.
    pub id: String,
    /// Session display name.
    pub name: String,
    /// Why it needs the user (`Needs approval`, `Failed`, …).
    pub reason: &'static str,
}

fn reason_label(reason: AttentionReason) -> &'static str {
    match reason {
        AttentionReason::Approval => "Needs approval",
        AttentionReason::Recovery => "Needs recovery",
        AttentionReason::Failed => "Failed",
        AttentionReason::Disconnected => "Disconnected",
        AttentionReason::Attention => "Needs attention",
    }
}

/// Sessions needing the user, most urgent first (Fleet ordering).
pub fn attention_queue(snapshot: &Snapshot) -> Vec<AttentionItem> {
    fleet_cards(snapshot)
        .into_iter()
        .filter_map(|card| {
            card.attention.map(|reason| AttentionItem {
                id: card.id,
                name: card.name,
                reason: reason_label(reason),
            })
        })
        .collect()
}

/// The next session to visit: the first queue entry after `current`
/// (wrapping), or the head when `current` is not queued. `None` when
/// nobody needs the user.
pub fn next_attention<'a>(
    queue: &'a [AttentionItem],
    current: Option<&str>,
) -> Option<&'a AttentionItem> {
    if queue.is_empty() {
        return None;
    }
    match current.and_then(|id| queue.iter().position(|item| item.id == id)) {
        Some(index) => Some(&queue[(index + 1) % queue.len()]),
        None => queue.first(),
    }
}

/// Compact token formatting for the top bar (`950`, `34k`, `1.2M`).
pub fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EngineStatus, EngineTelemetry, RunState, Session, Workspace};

    fn session(id: &str, backend: Backend, state: RunState) -> Session {
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
            state,
            recovery_required: false,
            generation: 0,
            archived: false,
            position: 0,
            agent_session: None,
            resumable: false,
            approvals: Vec::new(),
        }
    }

    fn local() -> Backend {
        Backend::Local {
            executable: "plank".into(),
            args: vec![],
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

    fn with_status(snap: &mut Snapshot, id: &str, generated: u64, ctx_used: u64, ctx_size: u64) {
        snap.telemetry.insert(
            id.into(),
            EngineTelemetry {
                status: Some(EngineStatus {
                    state: "running".into(),
                    ctx_used,
                    ctx_size,
                    generated,
                    gen_tps: 10.0,
                    elapsed_secs: 5.0,
                    think: String::new(),
                }),
                ..EngineTelemetry::default()
            },
        );
    }

    #[test]
    fn usage_aggregates_tokens_and_peak_context() {
        let mut snap = snapshot(vec![
            session("a", local(), RunState::Running),
            session("b", local(), RunState::Idle),
            session("c", Backend::Demo, RunState::Idle),
        ]);
        with_status(&mut snap, "a", 1_500_000, 50, 100);
        with_status(&mut snap, "b", 500_000, 90, 100);
        let usage = provider_usage(&snap);
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].label, "Local Plank");
        assert_eq!(usage[0].sessions, 2);
        assert_eq!(usage[0].running, 1);
        assert_eq!(usage[0].generated, 2_000_000);
        assert_eq!(usage[0].ctx_peak, Some(90));
        assert_eq!(format_tokens(usage[0].generated), "2.0M");
    }

    #[test]
    fn attention_queue_orders_approvals_first_and_cycles() {
        let mut needy = session("needy", local(), RunState::Idle);
        needy.approvals.push(crate::model::PendingApproval {
            id: "p1".into(),
            requirement: serde_json::json!({}),
            tool: "edit".into(),
            arguments: "{}".into(),
            document: None,
            choices: Vec::new(),
        });
        let mut failed = session("failed", local(), RunState::Failed);
        failed.attention = true;
        let snap = snapshot(vec![failed, needy]);
        let queue = attention_queue(&snap);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].id, "needy");
        assert_eq!(queue[0].reason, "Needs approval");
        // Cycling wraps from the last entry back to the head.
        assert_eq!(
            next_attention(&queue, Some("needy")).map(|i| i.id.as_str()),
            Some("failed")
        );
        assert_eq!(
            next_attention(&queue, Some("failed")).map(|i| i.id.as_str()),
            Some("needy")
        );
        assert_eq!(
            next_attention(&queue, None).map(|i| i.id.as_str()),
            Some("needy")
        );
        assert!(next_attention(&[], None).is_none());
    }
}
