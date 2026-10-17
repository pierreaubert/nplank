//! CI runs board: recent workflow runs for the workspace repository,
//! fetched through the `gh` (GitHub) or `tea` (Gitea) CLIs the user is
//! already authenticated with. Pure parsing lives here and is tested;
//! process execution is a thin wrapper.

use std::path::Path;
use std::process::Command;

/// Normalised run state across forges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    Success,
    Failure,
    Running,
    Queued,
    Cancelled,
    Other,
}

impl CiState {
    /// Short status glyph for list rows.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Success => "✓",
            Self::Failure => "✗",
            Self::Running => "●",
            Self::Queued => "◌",
            Self::Cancelled => "⊘",
            Self::Other => "?",
        }
    }

    fn from_parts(status: &str, conclusion: &str) -> Self {
        match status {
            "queued" | "waiting" | "pending" => Self::Queued,
            "in_progress" | "running" => Self::Running,
            _ => match conclusion {
                "success" => Self::Success,
                "failure" | "timed_out" | "startup_failure" => Self::Failure,
                "cancelled" | "canceled" | "skipped" => Self::Cancelled,
                _ => Self::Other,
            },
        }
    }
}

/// One workflow run row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiRun {
    /// Workflow display name.
    pub workflow: String,
    /// Normalised state.
    pub state: CiState,
    /// Head branch.
    pub branch: String,
    /// Triggering event (`push`, `pull_request`, …).
    pub event: String,
    /// Web URL of the run.
    pub url: String,
    /// Start time, RFC 3339 as reported by the forge.
    pub started: String,
    /// Human duration (`14s`, `3m 05s`) when both timestamps exist.
    pub duration: Option<String>,
}

/// Which forge CLI serves a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiProvider {
    GitHub,
    Gitea,
}

impl CiProvider {
    /// Display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::Gitea => "Gitea",
        }
    }
}

/// Detects the forge from a git remote URL: github.com is GitHub,
/// anything else is assumed to be a Gitea instance (tea's remit).
pub fn detect_provider(remote: &str) -> Option<CiProvider> {
    let host = remote
        .split("://")
        .nth(1)
        .unwrap_or(remote)
        .split(['/', ':', '?'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else if host == "github.com" {
        Some(CiProvider::GitHub)
    } else {
        Some(CiProvider::Gitea)
    }
}

fn field<'a>(value: &'a serde_json::Value, names: &[&str]) -> &'a str {
    names
        .iter()
        .filter_map(|name| value.get(name).and_then(serde_json::Value::as_str))
        .find(|text| !text.is_empty())
        .unwrap_or("")
}

fn parse_runs(value: &serde_json::Value) -> Vec<CiRun> {
    let items = match value {
        serde_json::Value::Array(items) => items.clone(),
        other => other
            .get("workflow_runs")
            .or_else(|| other.get("runs"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
    };
    items
        .iter()
        .map(|item| {
            let status = field(item, &["status"]);
            let conclusion = field(item, &["conclusion"]);
            let started = field(
                item,
                &["createdAt", "created_at", "run_started_at", "started_at"],
            );
            let updated = field(item, &["updatedAt", "updated_at", "completed_at"]);
            CiRun {
                workflow: field(item, &["name", "workflowName", "workflow_name", "title"])
                    .to_owned(),
                state: CiState::from_parts(status, conclusion),
                branch: field(item, &["headBranch", "head_branch", "branch"]).to_owned(),
                event: field(item, &["event"]).to_owned(),
                url: field(item, &["url", "html_url"]).to_owned(),
                started: started.to_owned(),
                duration: duration_between(started, updated),
            }
        })
        .filter(|run| !run.workflow.is_empty() || !run.url.is_empty())
        .collect()
}

/// Parses `gh run list --json …` output.
pub fn parse_gh_runs(json: &str) -> Result<Vec<CiRun>, String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| format!("gh output was not JSON: {error}"))?;
    Ok(parse_runs(&value))
}

/// Parses `tea actions runs list -o json` output (field aliases tolerated).
pub fn parse_tea_runs(output: &str) -> Result<Vec<CiRun>, String> {
    let trimmed = output.trim();
    if trimmed.is_empty() || trimmed.contains("No workflow runs found") {
        return Ok(Vec::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|error| format!("tea output was not JSON: {error}"))?;
    Ok(parse_runs(&value))
}

/// Parses an RFC 3339 timestamp to Unix seconds (no external date crate).
fn rfc3339_to_unix(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let num = |from: usize, len: usize| -> Option<i64> {
        std::str::from_utf8(&bytes[from..from + len])
            .ok()?
            .parse()
            .ok()
    };
    let (year, month, day) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (hour, min, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    // Civil-date to days algorithm (Howard Hinnant's).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3_600 + min * 60 + sec)
}

/// `14s`, `3m 05s`, `1h 02m` between two RFC 3339 timestamps.
fn duration_between(start: &str, end: &str) -> Option<String> {
    let secs = rfc3339_to_unix(end)? - rfc3339_to_unix(start)?;
    if secs < 0 {
        return None;
    }
    Some(format_duration(secs))
}

/// Compact duration formatting.
pub fn format_duration(secs: i64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3_600 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h {:02}m", secs / 3_600, (secs % 3_600) / 60)
    }
}

/// `just now`, `5m ago`, `3h ago`, `2d ago` for an RFC 3339 start time.
pub fn relative_age(started: &str, now_unix: i64) -> String {
    let Some(start) = rfc3339_to_unix(started) else {
        return String::new();
    };
    let age = (now_unix - start).max(0);
    if age < 60 {
        "just now".into()
    } else if age < 3_600 {
        format!("{}m ago", age / 60)
    } else if age < 86_400 {
        format!("{}h ago", age / 3_600)
    } else {
        format!("{}d ago", age / 86_400)
    }
}

/// Fetches recent runs for the repository at `directory` (blocking;
/// call off the UI thread).
pub fn fetch_runs(directory: &Path) -> Result<(CiProvider, Vec<CiRun>), String> {
    let remote = run_command(
        Command::new("git")
            .args(["remote", "get-url", "origin"])
            .current_dir(directory),
    )?;
    let provider = detect_provider(remote.trim())
        .ok_or_else(|| "Could not determine the forge from the origin remote".to_owned())?;
    match provider {
        CiProvider::GitHub => {
            let output = run_command(
                Command::new("gh")
                    .args([
                        "run",
                        "list",
                        "--limit",
                        "25",
                        "--json",
                        "databaseId,name,status,conclusion,createdAt,updatedAt,url,event,headBranch",
                    ])
                    .current_dir(directory),
            )?;
            Ok((provider, parse_gh_runs(&output)?))
        }
        CiProvider::Gitea => {
            let output = run_command(
                Command::new("tea")
                    .args(["actions", "runs", "list", "--limit", "25", "-o", "json"])
                    .current_dir(directory),
            )?;
            Ok((provider, parse_tea_runs(&output)?))
        }
    }
}

fn run_command(command: &mut Command) -> Result<String, String> {
    let output = command.output().map_err(|error| {
        format!(
            "Could not run {}: {error}",
            command.get_program().to_string_lossy()
        )
    })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("command failed").trim();
        Err(first.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_detection_from_remotes() {
        assert_eq!(
            detect_provider("git@github.com:pierreaubert/nplank.git"),
            Some(CiProvider::GitHub)
        );
        assert_eq!(
            detect_provider("https://github.com/a/b"),
            Some(CiProvider::GitHub)
        );
        assert_eq!(
            detect_provider("http://192.168.1.32:3001/pierre/all_of_sonium"),
            Some(CiProvider::Gitea)
        );
        assert_eq!(detect_provider(""), None);
    }

    #[test]
    fn gh_runs_parse_with_duration() {
        let runs = parse_gh_runs(
            r#"[{"conclusion":"failure","createdAt":"2026-10-15T10:49:28Z","databaseId":1,"event":"push","headBranch":"main","name":"Rust","status":"completed","updatedAt":"2026-10-15T10:49:42Z","url":"https://example/1"},
                {"conclusion":"","createdAt":"2026-10-15T11:00:00Z","databaseId":2,"event":"pull_request","headBranch":"feat","name":"Rust","status":"in_progress","updatedAt":"2026-10-15T11:00:30Z","url":"https://example/2"}]"#,
        )
        .expect("gh json");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].state, CiState::Failure);
        assert_eq!(runs[0].duration.as_deref(), Some("14s"));
        assert_eq!(runs[1].state, CiState::Running);
        assert_eq!(
            relative_age("2026-10-15T10:49:28Z", 1_792_061_368 + 3_600),
            "1h ago"
        );
    }

    #[test]
    fn tea_runs_tolerate_api_field_names() {
        let runs = parse_tea_runs(
            r#"{"workflow_runs":[{"id":7,"name":"build","status":"completed","conclusion":"success","head_branch":"main","event":"push","html_url":"https://gitea/7","created_at":"2026-10-15T09:00:00Z","updated_at":"2026-10-15T09:03:05Z"}]}"#,
        )
        .expect("tea json");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].state, CiState::Success);
        assert_eq!(runs[0].duration.as_deref(), Some("3m 05s"));
        assert!(
            parse_tea_runs("No workflow runs found")
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn rfc3339_epoch_math() {
        assert_eq!(rfc3339_to_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(rfc3339_to_unix("2026-10-15T10:49:28Z"), Some(1_792_061_368));
        assert_eq!(format_duration(3_723), "1h 02m");
    }
}
