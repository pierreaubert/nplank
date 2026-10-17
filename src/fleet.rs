//! Glanceable fleet-overview cards derived from a daemon snapshot.
//!
//! Pure derivation: the view renders the cards, tests pin the ordering and
//! attention rules. Context and token counters come from
//! `EngineTelemetry.status`; `SessionContext` is git state and unused here.

// Rust guideline compliant 2026-02-21
use crate::model::{RunState, Snapshot};

/// Why a session needs the user before it can proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionReason {
    /// A tool call is waiting on `PendingApproval` choices.
    Approval,
    /// The engine asked for recovery before continuing.
    Recovery,
    /// The run failed.
    Failed,
    /// The engine transport dropped.
    Disconnected,
    /// The engine raised its generic attention flag.
    Attention,
}

impl AttentionReason {
    /// Urgency rank: a run blocked on your approval first, generic
    /// attention flags last.
    pub fn rank(self) -> u8 {
        match self {
            Self::Approval => 0,
            Self::Recovery => 1,
            Self::Failed => 2,
            Self::Disconnected => 3,
            Self::Attention => 4,
        }
    }
}

/// One fleet-overview card: everything the grid renders, precomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionCard {
    /// Session id, used for click-to-select.
    pub id: String,
    /// Session display name.
    pub name: String,
    /// Owning workspace, shown as a muted label.
    pub workspace_id: String,
    /// Run state label (`Running`, `Ready`, …).
    pub state_label: &'static str,
    /// Whether a prompt is executing.
    pub busy: bool,
    /// Attention reason, if any; attention cards sort first.
    pub attention: Option<AttentionReason>,
    /// `EngineTelemetry::summary()` line, or the state label as fallback.
    pub status_line: String,
    /// Context fill `0..=100` when the engine reports a window size.
    pub ctx_percent: Option<u8>,
    /// Engine-reported elapsed seconds for the current run.
    pub elapsed_secs: Option<f64>,
    /// Sidebar position, the stable tiebreaker for ordering.
    pub position: i64,
    /// One-line session summary, shown in the card's live miniature.
    pub summary: String,
}

/// Human label for one run state.
fn state_label(state: RunState) -> &'static str {
    match state {
        RunState::Idle => "Ready",
        RunState::Starting => "Starting",
        RunState::Running => "Running",
        RunState::Disconnected => "Disconnected",
        RunState::Interrupted => "Interrupted",
        RunState::Failed => "Failed",
    }
}

/// Derives fleet cards from a snapshot: attention first, then busy, then
/// sidebar position. Archived sessions never appear.
pub fn fleet_cards(snapshot: &Snapshot) -> Vec<SessionCard> {
    let mut cards: Vec<SessionCard> = snapshot
        .sessions
        .iter()
        .filter(|session| !session.archived)
        .map(|session| {
            let telemetry = snapshot.telemetry.get(&session.id);
            let status = telemetry.and_then(|telemetry| telemetry.status.as_ref());
            let attention = if !session.approvals.is_empty() {
                Some(AttentionReason::Approval)
            } else if session.recovery_required {
                Some(AttentionReason::Recovery)
            } else if session.state == RunState::Failed {
                Some(AttentionReason::Failed)
            } else if session.state == RunState::Disconnected {
                Some(AttentionReason::Disconnected)
            } else if session.attention {
                Some(AttentionReason::Attention)
            } else {
                None
            };
            let status_line = telemetry.map_or_else(
                || state_label(session.state).to_owned(),
                |telemetry| {
                    let summary = telemetry.summary();
                    if summary.is_empty() {
                        state_label(session.state).to_owned()
                    } else {
                        summary
                    }
                },
            );
            SessionCard {
                id: session.id.clone(),
                name: session.name.clone(),
                workspace_id: session.workspace_id.clone(),
                state_label: state_label(session.state),
                busy: session.state.busy(),
                attention,
                status_line,
                ctx_percent: status.and_then(|status| {
                    (status.ctx_size > 0).then(|| {
                        ((status.ctx_used.saturating_mul(100)) / status.ctx_size).min(100) as u8
                    })
                }),
                elapsed_secs: status.map(|status| status.elapsed_secs),
                position: session.position,
                summary: session.summary.clone(),
            }
        })
        .collect();
    cards.sort_by_key(|card| {
        (
            card.attention.map_or(5, AttentionReason::rank),
            !card.busy,
            card.position,
        )
    });
    cards
}

/// Formats elapsed seconds as `12s`, `12m 41s`, or `1h 03m`.
pub fn format_elapsed(elapsed_secs: f64) -> String {
    let total = elapsed_secs.max(0.0) as u64;
    if total < 60 {
        format!("{total}s")
    } else if total < 3600 {
        format!("{}m {:02}s", total / 60, total % 60)
    } else {
        format!("{}h {:02}m", total / 3600, (total % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::{AttentionReason, fleet_cards, format_elapsed};
    use crate::model::{Backend, EngineStatus, EngineTelemetry, RunState, Session, Snapshot};
    use std::collections::HashMap;

    fn session(id: &str, position: i64, state: RunState) -> Session {
        Session {
            worktree_id: None,
            progress: None,
            controls: None,
            attention: false,
            started_at: None,
            completed_at: None,
            summary: String::new(),
            id: id.into(),
            workspace_id: "ws".into(),
            name: format!("Session {id}"),
            backend: Backend::Demo,
            state,
            recovery_required: false,
            generation: 0,
            archived: false,
            position,
            agent_session: None,
            resumable: false,
            approvals: Vec::new(),
        }
    }

    fn snapshot(sessions: Vec<Session>) -> Snapshot {
        Snapshot {
            sessions,
            ..Snapshot::default()
        }
    }

    #[test]
    fn archived_sessions_are_excluded_and_empty_stays_empty() {
        assert!(fleet_cards(&Snapshot::default()).is_empty());
        let mut archived = session("old", 0, RunState::Idle);
        archived.archived = true;
        let cards = fleet_cards(&snapshot(vec![archived]));
        assert!(cards.is_empty());
    }

    #[test]
    fn attention_beats_busy_beats_position() {
        let mut needs_approval = session("c-third", 2, RunState::Idle);
        needs_approval
            .approvals
            .push(crate::model::PendingApproval {
                id: "a1".into(),
                requirement: serde_json::Value::Null,
                tool: "shell".into(),
                arguments: String::new(),
                document: None,
                choices: Vec::new(),
            });
        let cards = fleet_cards(&snapshot(vec![
            session("a-first", 0, RunState::Running),
            session("b-second", 1, RunState::Idle),
            needs_approval,
        ]));
        let ids: Vec<&str> = cards.iter().map(|card| card.id.as_str()).collect();
        assert_eq!(ids, ["c-third", "a-first", "b-second"]);
        assert_eq!(cards[0].attention, Some(AttentionReason::Approval));
        assert!(cards[1].busy);
        assert_eq!(cards[1].state_label, "Running");
    }

    #[test]
    fn telemetry_fills_status_ctx_and_elapsed() {
        let mut snap = snapshot(vec![session("s", 0, RunState::Running)]);
        snap.telemetry = HashMap::from([(
            "s".to_owned(),
            EngineTelemetry {
                status: Some(EngineStatus {
                    state: "Generating".into(),
                    ctx_used: 41,
                    ctx_size: 100,
                    generated: 900,
                    gen_tps: 32.0,
                    elapsed_secs: 761.0,
                    think: String::new(),
                }),
                ..EngineTelemetry::default()
            },
        )]);
        let cards = fleet_cards(&snap);
        assert_eq!(cards[0].ctx_percent, Some(41));
        assert_eq!(cards[0].elapsed_secs, Some(761.0));
        assert!(cards[0].status_line.contains("32.0 tok/s"));
    }

    #[test]
    fn missing_telemetry_falls_back_to_state_label() {
        let cards = fleet_cards(&snapshot(vec![session("s", 0, RunState::Failed)]));
        assert_eq!(cards[0].status_line, "Failed");
        assert_eq!(cards[0].attention, Some(AttentionReason::Failed));
        assert_eq!(cards[0].ctx_percent, None);
    }

    #[test]
    fn elapsed_formatting_boundaries() {
        assert_eq!(format_elapsed(0.0), "0s");
        assert_eq!(format_elapsed(59.9), "59s");
        assert_eq!(format_elapsed(761.0), "12m 41s");
        assert_eq!(format_elapsed(3780.0), "1h 03m");
        assert_eq!(format_elapsed(-5.0), "0s");
    }

    #[test]
    fn navigation_status_tracks_user_work_including_hidden_sessions() {
        use crate::navigation::{AttentionLevel, attention_summary};
        let mut sessions = vec![session("running", 0, RunState::Running)];
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Clear, 0)
        );
        let mut pending = session("other-workspace", 1, RunState::Idle);
        pending.workspace_id = "hidden-workspace".into();
        pending.attention = true;
        sessions.push(pending);
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Pending, 1)
        );
        sessions.push(session("failure", 2, RunState::Failed));
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Required, 2)
        );
        sessions[2].archived = true;
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Pending, 1)
        );
        sessions[0].state = RunState::Disconnected;
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Unknown, 2)
        );
        assert_eq!(
            attention_summary(&sessions, false),
            (AttentionLevel::Unknown, 0)
        );
        sessions[0].recovery_required = true;
        assert_eq!(
            attention_summary(&sessions, true),
            (AttentionLevel::Required, 2)
        );
    }
}
