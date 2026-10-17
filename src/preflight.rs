//! Preflight data for starting a run: what will execute, how full the
//! context is, and which risk signals deserve a confirmation first.
//!
//! Estimates are client-side heuristics (bytes/4 for tokens). The panel
//! informs and confirms; it never decides.

// Rust guideline compliant 2026-02-21
use crate::model::{EngineTelemetry, Session};

/// Everything the preflight panel renders, precomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct PreflightInfo {
    /// Human backend label (`Local Plank`, model name, …).
    pub backend: String,
    /// Context fill `0..=100` when the engine reports a window.
    pub ctx_percent: Option<u8>,
    /// Heuristic token estimate for the draft text.
    pub estimated_tokens: usize,
    /// Total size of attached files in bytes.
    pub attachment_bytes: u64,
    /// Risk signals worth showing before the run starts.
    pub warnings: Vec<String>,
}

/// Rough token estimate for prompt text (4 bytes per token, rounded up).
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Builds the panel data for one session about to receive a prompt.
pub fn preflight_info(
    session: &Session,
    telemetry: Option<&EngineTelemetry>,
    draft: &str,
    attachment_bytes: u64,
    backend_label: &str,
) -> PreflightInfo {
    let status = telemetry.and_then(|telemetry| telemetry.status.as_ref());
    let ctx_percent = status.and_then(|status| {
        (status.ctx_size > 0)
            .then(|| ((status.ctx_used.saturating_mul(100)) / status.ctx_size).min(100) as u8)
    });
    let estimated_tokens = estimate_tokens(draft);
    let mut warnings = Vec::new();
    if let Some(percent) = ctx_percent
        && percent >= 85
    {
        warnings.push(format!(
            "Context is {percent}% full; the run may truncate earlier history."
        ));
    }
    if estimated_tokens >= 4000 {
        warnings.push(format!(
            "Large prompt (~{estimated_tokens} estimated tokens before history)."
        ));
    }
    if attachment_bytes >= 1_000_000 {
        warnings
            .push("Attachments total over 1 MB; the engine reads them into context.".to_owned());
    }
    let _ = session;
    PreflightInfo {
        backend: backend_label.to_owned(),
        ctx_percent,
        estimated_tokens,
        attachment_bytes,
        warnings,
    }
}

/// Whether the panel should appear before this prompt is sent.
pub fn needs_preflight(info: &PreflightInfo) -> bool {
    !info.warnings.is_empty()
}

#[cfg(test)]
mod tests {
    use super::{estimate_tokens, needs_preflight, preflight_info};
    use crate::model::{Backend, EngineStatus, EngineTelemetry, RunState, Session};

    fn session() -> Session {
        Session {
            worktree_id: None,
            progress: None,
            controls: None,
            attention: false,
            started_at: None,
            completed_at: None,
            summary: String::new(),
            id: "s".into(),
            workspace_id: "ws".into(),
            name: "Agent".into(),
            backend: Backend::Demo,
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

    fn telemetry(ctx_used: u64, ctx_size: u64) -> EngineTelemetry {
        EngineTelemetry {
            status: Some(EngineStatus {
                state: "Ready".into(),
                ctx_used,
                ctx_size,
                generated: 0,
                gen_tps: 0.0,
                elapsed_secs: 0.0,
                think: String::new(),
            }),
            ..EngineTelemetry::default()
        }
    }

    #[test]
    fn quiet_prompts_skip_the_panel() {
        let session = session();
        let info = preflight_info(&session, None, "hello", 0, "Demo");
        assert_eq!(info.estimated_tokens, 2);
        assert!(!needs_preflight(&info));
    }

    #[test]
    fn full_context_and_large_inputs_warn() {
        let session = session();
        let telemetry = telemetry(90, 100);
        let info = preflight_info(&session, Some(&telemetry), "hi", 0, "Demo");
        assert_eq!(info.ctx_percent, Some(90));
        assert!(needs_preflight(&info));
        assert!(info.warnings[0].contains("90%"));

        let big = "x".repeat(16_004);
        let info = preflight_info(&session, None, &big, 0, "Demo");
        assert!(needs_preflight(&info));

        let info = preflight_info(&session, None, "hi", 1_500_000, "Demo");
        assert!(needs_preflight(&info));
    }

    #[test]
    fn token_estimate_rounds_up() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }
}
