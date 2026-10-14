//! Serializable workspace and session state shared by clients and the daemon.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Serialize};

/// Client/daemon protocol version, including required remote input ownership fencing.
pub const PROTOCOL_VERSION: u32 = 28;

/// A named collection of sessions with a working directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub directory: String,
}

/// Supported third-party coding-agent protocols.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentProvider {
    MuseCode,
    Codex,
    ClaudeCode,
}

impl AgentProvider {
    /// Returns the agent's display name.
    pub fn label(self) -> &'static str {
        match self {
            Self::MuseCode => "Muse Code",
            Self::Codex => "Codex",
            Self::ClaudeCode => "Claude Code",
        }
    }
    /// Returns the conventional executable name.
    pub fn executable(self) -> &'static str {
        match self {
            Self::MuseCode => "muse",
            Self::Codex => "codex",
            Self::ClaudeCode => "claude",
        }
    }
}

/// A runner's availability on the daemon host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider: AgentProvider,
    pub executable: Option<String>,
}

/// A provider-supplied approval choice, retained without changing its semantics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalChoice {
    pub id: String,
    pub label: String,
}

/// An outstanding provider approval or Plank question with a requirement revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApproval {
    pub id: String,
    pub requirement: serde_json::Value,
    pub tool: String,
    pub arguments: String,
    pub choices: Vec<ApprovalChoice>,
}

impl PendingApproval {
    /// Validates an explicit choice without broadening a provider's permissions.
    pub fn accepts_choice(&self, choice: &str) -> bool {
        if self.choices.iter().any(|option| option.id == choice) {
            return true;
        }
        self.requirement["kind"] == "plank_question"
            && self.requirement["request"]["multi"] == true
            && !choice.is_empty()
            && choice
                .split(',')
                .all(|id| id != "0" && self.choices.iter().any(|option| option.id == id))
    }
}

/// Execution location for an agent session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Backend {
    Demo,
    Local {
        executable: String,
        args: Vec<String>,
    },
    Remote {
        url: String,
        token_env: String,
    },
    Agent {
        provider: AgentProvider,
        executable: String,
        args: Vec<String>,
        model: Option<String>,
    },
}

/// Observable execution state, independent of any attached window.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Starting,
    Running,
    Disconnected,
    Interrupted,
    Failed,
}

impl RunState {
    /// Indicates whether a prompt is currently executing.
    pub fn busy(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

/// A durable session associated with one workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub progress: Option<EngineProgress>,
    #[serde(default)]
    pub controls: Option<crate::commands::EngineControls>,
    #[serde(default)]
    pub attention: bool,
    #[serde(default)]
    pub started_at: Option<u64>,
    #[serde(default)]
    pub completed_at: Option<u64>,
    #[serde(default)]
    pub summary: String,
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub backend: Backend,
    pub state: RunState,
    pub recovery_required: bool,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub agent_session: Option<String>,
    #[serde(default)]
    pub resumable: bool,
    #[serde(default)]
    pub approvals: Vec<PendingApproval>,
}

/// Transient engine loading or download progress, independent of transcript output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineProgress {
    pub label: String,
    pub current: u64,
    pub total: u64,
}

/// One ordered transcript entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub sequence: i64,
    pub session_id: String,
    pub role: String,
    pub text: String,
}

/// A workspace snapshot with a bounded transcript page.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub telemetry: std::collections::HashMap<String, EngineTelemetry>,
    #[serde(default)]
    pub contexts: std::collections::HashMap<String, SessionContext>,
    #[serde(default)]
    pub protocol_version: u32,
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub providers: Vec<ProviderInfo>,
}

/// Live engine counters, kept outside the durable conversation database.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineTelemetry {
    /// Most recent complete, checksum-verified export in this worker generation.
    #[serde(default)]
    pub export: Option<EngineExport>,
    /// Latest complete standalone HTML report in this worker generation.
    #[serde(default)]
    pub insights_export: Option<EngineExport>,
    pub status: Option<EngineStatus>,
    pub tasks: Option<(u64, u64)>,
    #[serde(default)]
    pub task_report: Option<String>,
    #[serde(default)]
    pub task_rows: Option<Vec<EngineTask>>,
    #[serde(default)]
    pub goal_label: Option<String>,
    #[serde(default)]
    pub jobs: Option<EngineJobs>,
    #[serde(default)]
    pub saved_sessions: Option<EngineSavedSessions>,
    #[serde(default)]
    pub config: Option<EngineConfig>,
    #[serde(default)]
    pub checkpoints: Option<EngineHistory>,
    #[serde(default)]
    pub branches: Option<EngineHistory>,
    #[serde(default)]
    pub extensions: std::collections::BTreeMap<String, EngineExtensions>,
    #[serde(default)]
    pub cache: Option<EngineCache>,
    #[serde(default)]
    pub downloads: Option<EngineDownloads>,
    #[serde(default)]
    pub metrics: std::collections::BTreeMap<String, EngineMetrics>,
    #[serde(default)]
    pub host: Option<EngineHost>,
    #[serde(default)]
    pub remote_access: Option<RemoteAccess>,
}

/// Metadata for a verified export; file bytes are retrieved separately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineExport {
    #[serde(default)]
    pub purpose: ExportPurpose,
    pub id: String,
    pub name: String,
    pub format: String,
    pub bytes: u64,
    pub sha256: String,
}

/// Identifies the view that owns a downloadable artifact.
#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExportPurpose {
    #[default]
    Conversation,
    Insights,
}

impl EngineExport {
    /// Matches a complete Insights artifact to the currently displayed report.
    pub fn matches_insights(&self, metrics: &EngineMetrics) -> bool {
        self.is_valid()
            && self.purpose == ExportPurpose::Insights
            && matches!(metrics, EngineMetrics::Insights { html_sha256: Some(hash), .. } if hash == &self.sha256)
    }
    /// Validates bounded metadata before requesting or saving an export.
    pub fn is_valid(&self) -> bool {
        uuid::Uuid::parse_str(&self.id).is_ok()
            && !self.name.is_empty()
            && self.name.len() <= 255
            && self.name != "."
            && self.name != ".."
            && !self
                .name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
            && matches!(self.format.as_str(), "md" | "html")
            && (self.purpose != ExportPurpose::Insights || self.format == "html")
            && self.sha256.len() == 64
            && self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// Returns a suggested filename compatible with desktop and browser downloads.
    pub fn suggested_name(&self) -> String {
        let name: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_control()
                    || matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
                {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let name = name.trim_end_matches(['.', ' ']);
        let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        if name.is_empty() || reserved {
            format!(
                "session.{}",
                if self.format == "html" { "html" } else { "md" }
            )
        } else {
            name.into()
        }
    }
}

/// Live attachment role, including an input held until a grant arrives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteAccess {
    pub client_id: Option<u64>,
    pub controller: bool,
    pub requesting: bool,
    pub waiting_input: bool,
    pub reason: Option<String>,
}

/// Engine-host endpoint and control policy, without authentication credentials.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineHost {
    pub endpoint: Option<String>,
    pub holder: String,
    pub controller: Option<u64>,
    pub automatic: bool,
    pub reserved: bool,
    pub pending: Vec<u64>,
    pub pending_total: u64,
}

impl EngineHost {
    /// Validates loopback endpoints, policy states, and bounded waiting IDs.
    pub fn is_valid(&self) -> bool {
        let valid_endpoint = self.endpoint.as_deref().is_some_and(|endpoint| {
            endpoint.len() <= 256
                && endpoint
                    .strip_prefix("ws://")
                    .and_then(|address| address.strip_suffix('/'))
                    .and_then(|address| address.parse::<std::net::SocketAddr>().ok())
                    .is_some_and(|address| address.ip().is_loopback() && address.port() > 0)
        });
        if self.holder == "off" {
            return self.endpoint.is_none()
                && self.controller.is_none()
                && !self.automatic
                && !self.reserved
                && self.pending_total == 0
                && self.pending.is_empty();
        }
        let mut ids = std::collections::HashSet::new();
        valid_endpoint
            && matches!(self.holder.as_str(), "local" | "remote" | "free")
            && (self.controller.is_some() == (self.holder == "remote"))
            && (!self.reserved || self.controller.is_some())
            && (self.pending_total == 0 || (self.holder == "local" && !self.automatic))
            && self.pending.len() as u64 == self.pending_total.min(64)
            && self
                .pending
                .iter()
                .all(|id| Some(*id) != self.controller && ids.insert(*id))
    }
}

/// One task in the engine's current conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineTask {
    pub id: u32,
    pub subject: String,
    pub status: String,
    pub active_form: Option<String>,
}

impl EngineTask {
    /// Validates stable positive IDs, bounded text and known lifecycle states.
    pub fn is_valid(&self) -> bool {
        let label = |text: &str| text.len() <= 512 && !text.chars().any(char::is_control);
        self.id > 0
            && label(&self.subject)
            && matches!(
                self.status.as_str(),
                "pending" | "in_progress" | "completed"
            )
            && self.active_form.as_deref().is_none_or(label)
    }
}

/// Engine-host measurements, independently retained by category.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineMetrics {
    Subagents {
        rows: Vec<EngineSubagent>,
        total: u64,
        running: u64,
    },
    Insights {
        markdown: String,
        #[serde(default)]
        html_sha256: Option<String>,
    },
    Engines {
        snapshot: EngineInventory,
    },
    Activity {
        snapshot: ActivitySnapshot,
    },
    Context {
        model: String,
        window: u64,
        used: u64,
        categories: [u64; 5],
    },
    Usage {
        model: String,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        provider_passes: Option<u64>,
        wall_secs: f64,
    },
    Throughput {
        generation: Vec<f64>,
        prefill: Vec<f64>,
    },
}

impl EngineMetrics {
    /// Returns the metadata category used for replacement and display.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Subagents { .. } => "subagents",
            Self::Insights { .. } => "insights",
            Self::Engines { .. } => "engines",
            Self::Activity { .. } => "activity",
            Self::Context { .. } => "context",
            Self::Usage { .. } => "usage",
            Self::Throughput { .. } => "throughput",
        }
    }

    /// Validates bounded labels and finite, nonnegative measurements.
    pub fn is_valid(&self) -> bool {
        let valid_model = |model: &str| model.len() <= 256 && !model.chars().any(char::is_control);
        match self {
            Self::Subagents {
                rows,
                total,
                running,
            } => {
                let mut ids = std::collections::HashSet::new();
                let legacy = rows.iter().all(|row| {
                    row.id == 0
                        && row.report.is_none()
                        && row.live.is_none()
                        && row.output.is_none()
                        && !row.output_truncated
                });
                rows.len() as u64 == (*total).min(64)
                    && *running <= *total
                    && rows.iter().all(|row| {
                        row.label.len() <= 128
                            && row.task.len() <= 1024
                            && !row
                                .label
                                .chars()
                                .chain(row.task.chars())
                                .any(char::is_control)
                            && (legacy || (row.id > 0 && ids.insert(row.id)))
                            && row.report.as_deref().is_none_or(|report| {
                                report.len() <= 4096
                                    && report
                                        .chars()
                                        .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
                            })
                            && (!row.output_truncated || row.output.is_some())
                            && row.output.as_deref().is_none_or(|text| {
                                text.len() <= 4096
                                    && text
                                        .chars()
                                        .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
                            })
                            && row.live.as_ref().is_none_or(|live| {
                                row.running
                                    && live.prefill_done <= live.prefill_total
                                    && live.prefill_total <= i32::MAX as u64
                                    && live.generated <= i32::MAX as u64
                            })
                    })
                    && rows.iter().filter(|row| row.running).count() as u64 == (*running).min(64)
            }
            Self::Insights {
                markdown,
                html_sha256,
            } => {
                markdown.len() <= 64 * 1024
                    && html_sha256.as_ref().is_none_or(|hash| {
                        hash.len() == 64
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                    && markdown
                        .chars()
                        .all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            }
            Self::Engines { snapshot } => snapshot.is_valid(),
            Self::Activity { snapshot } => snapshot.is_valid(),
            Self::Context {
                model,
                window,
                used,
                categories,
            } => {
                valid_model(model)
                    && *window <= i32::MAX as u64
                    && *used <= i32::MAX as u64
                    && categories.iter().all(|count| *count <= i32::MAX as u64)
            }
            Self::Usage {
                model, wall_secs, ..
            } => valid_model(model) && wall_secs.is_finite() && *wall_secs >= 0.0,
            Self::Throughput {
                generation,
                prefill,
            } => [generation, prefill].iter().all(|series| {
                series.len() <= 64 && series.iter().all(|rate| rate.is_finite() && *rate >= 0.0)
            }),
        }
    }
}

/// Display metadata for one delegated run, separate from its parent transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSubagent {
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub live: Option<EngineSubagentLive>,
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub report: Option<String>,
    pub label: String,
    pub task: String,
    pub running: bool,
    pub elapsed_ms: u64,
    pub prefill: u64,
    pub generated: u64,
}

/// An unfinished delegated pass, separate from its completed token totals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSubagentLive {
    pub prefilling: bool,
    pub prefill_done: u64,
    pub prefill_total: u64,
    /// Streamed text pieces; providers may emit more than one token per piece.
    pub generated: u64,
}

/// Requested model catalog and availability measured on the engine host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineInventory {
    pub page: u64,
    pub total: u64,
    pub current: Option<String>,
    pub ram: Option<u64>,
    pub free_disk: Option<u64>,
    pub ds4: bool,
    pub gemma: bool,
    pub engines: Vec<InventoryEntry>,
}

/// One model's catalog description and availability reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub name: String,
    pub version: u32,
    pub notes: String,
    pub state: String,
    pub detail: String,
    pub missing_bytes: Option<u64>,
}

impl EngineInventory {
    /// Validates pagination, bounded labels, known states, and unique names.
    pub fn is_valid(&self) -> bool {
        let label = |text: &str, limit| text.len() <= limit && !text.chars().any(char::is_control);
        self.page >= 1
            && self.page <= self.total.div_ceil(32).max(1)
            && self.engines.len() <= 32
            && self.engines.len() as u64
                == self
                    .total
                    .saturating_sub((self.page - 1).saturating_mul(32))
                    .min(32)
            && self.current.as_ref().is_none_or(|name| label(name, 128))
            && self.engines.iter().all(|row| {
                !row.name.is_empty()
                    && label(&row.name, 128)
                    && label(&row.notes, 1024)
                    && label(&row.detail, 512)
                    && matches!(
                        row.state.as_str(),
                        "installed" | "download" | "downloading" | "staged" | "disabled"
                    )
            })
            && self.engines.iter().enumerate().all(|(index, row)| {
                self.engines[..index]
                    .iter()
                    .all(|other| other.name != row.name)
            })
    }
}

/// Engine-authored annual activity and scoped headline figures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    pub scope: String,
    pub days: Vec<ActivityDay>,
    pub summary: Vec<String>,
}

/// One calendar cell, dated in the engine host's local timezone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityDay {
    pub date: String,
    pub tokens: u64,
    pub level: Option<u8>,
}

impl ActivitySnapshot {
    /// Checks fixed calendar dimensions, safe labels, and ranked intensities.
    pub fn is_valid(&self) -> bool {
        matches!(self.scope.as_str(), "all" | "7" | "30")
            && self.days.len() == 53 * 7
            && self.days.iter().all(ActivityDay::is_valid)
            && self.days.windows(2).all(|pair| pair[0].date < pair[1].date)
            && self.summary.len() == 8
            && self
                .summary
                .iter()
                .all(|line| line.len() <= 256 && !line.chars().any(char::is_control))
    }
}

impl ActivityDay {
    fn is_valid(&self) -> bool {
        let date = self.date.as_bytes();
        if date.len() != 10
            || !date.iter().enumerate().all(|(index, byte)| {
                if index == 4 || index == 7 {
                    *byte == b'-'
                } else {
                    byte.is_ascii_digit()
                }
            })
        {
            return false;
        }
        let year = self.date[..4].parse::<u32>().unwrap_or(0);
        let month = self.date[5..7].parse::<u32>().unwrap_or(0);
        let day = self.date[8..].parse::<u32>().unwrap_or(0);
        let last = match month {
            4 | 6 | 9 | 11 => 30,
            2 if year.is_multiple_of(4)
                && (!year.is_multiple_of(100) || year.is_multiple_of(400)) =>
            {
                29
            }
            2 => 28,
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            _ => return false,
        };
        (1..=last).contains(&day)
            && match self.level {
                None | Some(0) => self.tokens == 0,
                Some(1..=4) => self.tokens > 0,
                Some(_) => false,
            }
    }
}

/// Detached model-update progress on the engine host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineDownloads {
    pub state: Option<ModelDownload>,
    pub pending: bool,
    pub cancel_requested: Option<String>,
}

/// Current artifact progress and helper phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelDownload {
    pub set: String,
    pub version: u32,
    pub current: String,
    pub index: u64,
    pub of: u64,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub rate_bps: u64,
    pub phase: String,
    #[serde(default)]
    pub partials_kept: Option<bool>,
    pub error: Option<String>,
}

impl ModelDownload {
    /// Whether Plank permits cancellation in this helper phase.
    pub fn can_cancel(&self) -> bool {
        matches!(
            self.phase.as_str(),
            "rehashing" | "downloading" | "verifying" | "unknown"
        )
    }

    /// Current artifact fraction, without treating verification as byte transfer.
    pub fn fraction(&self) -> Option<f32> {
        (self.phase == "downloading" && self.total_bytes > 0).then(|| {
            (self.done_bytes.min(self.total_bytes) as f64 / self.total_bytes as f64) as f32
        })
    }
}

impl EngineDownloads {
    /// Validates bounded text, artifact counts, and cancellation semantics.
    pub fn is_valid(&self) -> bool {
        self.state.as_ref().is_none_or(|state| {
            matches!(
                state.phase.as_str(),
                "rehashing"
                    | "downloading"
                    | "verifying"
                    | "staged"
                    | "failed"
                    | "cancelled"
                    | "unknown"
            ) && state.set.len() <= 256
                && state.current.len() <= 256
                && !state.set.chars().any(char::is_control)
                && !state.current.chars().any(char::is_control)
                && state.index <= state.of
                && state.done_bytes <= state.total_bytes
                && (state.partials_kept.is_none() || state.phase == "cancelled")
                && state
                    .error
                    .as_ref()
                    .is_none_or(|error| error.len() <= 4096 && !error.chars().any(char::is_control))
        }) && self.cancel_requested.as_ref().is_none_or(|request| {
            matches!(request.as_str(), "keep" | "delete")
                && self.state.as_ref().is_some_and(ModelDownload::can_cancel)
        })
    }
}

/// A page of cache lineage from the engine host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineCache {
    pub total: u64,
    pub page: u64,
    pub summary: String,
    pub rows: Vec<CacheRow>,
}

/// A cache blob with a validated command identity, or a read-only group row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheRow {
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub entry: Option<String>,
    pub depth: u64,
    pub label: String,
    pub detail: String,
    pub status: String,
    pub pinned: bool,
}

impl CacheRow {
    /// Prepares a cache action for explicit Send without using a display prefix.
    pub fn command(&self, action: &str) -> Option<String> {
        let fingerprint = self.fingerprint.as_deref()?;
        if fingerprint.is_empty()
            || fingerprint.len() > 4096
            || fingerprint
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || !matches!(action, "pin" | "unpin" | "rm")
        {
            return None;
        }
        if let Some(entry) = &self.entry {
            if entry.len() != 40
                || !entry
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return None;
            }
            Some(format!("/kvcache {action} --entry {entry} {fingerprint}"))
        } else {
            Some(format!("/kvcache {action} {fingerprint}"))
        }
    }
}

impl EngineCache {
    /// Checks page bounds, text sizes, and unique command identities.
    pub fn is_valid(&self) -> bool {
        let mut identities = std::collections::HashSet::new();
        self.page > 0
            && self.page <= self.total.div_ceil(32).max(1)
            && self.rows.len() as u64
                == self
                    .total
                    .saturating_sub((self.page - 1).saturating_mul(32))
                    .min(32)
            && self.summary.len() <= 512
            && !self.summary.chars().any(char::is_control)
            && self.rows.iter().all(|row| {
                row.depth <= 64
                    && row.label.len() <= 512
                    && row.detail.len() <= 1024
                    && row.status.len() <= 512
                    && !row.label.chars().any(char::is_control)
                    && !row.detail.chars().any(char::is_control)
                    && !row.status.chars().any(char::is_control)
                    && row.fingerprint.as_ref().is_none_or(|fingerprint| {
                        row.command("rm").is_some()
                            && identities.insert(match &row.entry {
                                Some(entry) => format!("entry:{entry}"),
                                None => format!("fingerprint:{fingerprint}"),
                            })
                    })
                    && (row.entry.is_none() || row.fingerprint.is_some())
            })
    }
}

/// One requested inventory from the engine host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineExtensions {
    pub kind: String,
    pub report: String,
    pub enabled: Option<bool>,
}

impl EngineExtensions {
    /// Checks category, report bounds, and session-switch semantics.
    pub fn is_valid(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "plugins" | "skills" | "templates" | "hooks" | "mcp" | "agents"
        ) && self.report.len() <= 65 * 1024
            && self
                .report
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
            && (matches!(self.kind.as_str(), "skills" | "hooks") || self.enabled.is_none())
    }
}

/// One bounded page of engine-owned conversation history controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineHistory {
    pub kind: String,
    pub total: u64,
    pub page: u64,
    pub rows: Vec<HistoryPoint>,
    pub report: String,
}

/// A named rollback point or numbered active-branch fork point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub key: Option<String>,
    pub label: String,
    pub detail: String,
}

impl HistoryPoint {
    /// Prepares a single-line rollback or fork command for explicit submission.
    pub fn command(&self, kind: &str) -> Option<String> {
        let key = self.key.as_deref()?;
        if key.is_empty()
            || key.len() > 4096
            || key.trim() != key
            || key.chars().any(char::is_control)
        {
            return None;
        }
        match kind {
            "checkpoints" => Some(format!("/rollback {key}")),
            "forks" if key.parse::<u64>().is_ok_and(|index| index > 0) => {
                Some(format!("/fork {key}"))
            }
            _ => None,
        }
    }
}

impl EngineHistory {
    /// Validates page bounds, command arguments, and preview size.
    pub fn is_valid(&self) -> bool {
        let mut keys = std::collections::HashSet::new();
        matches!(self.kind.as_str(), "checkpoints" | "forks")
            && self.page > 0
            && self.page <= self.total.div_ceil(32).max(1)
            && self.rows.len() as u64
                == self
                    .total
                    .saturating_sub((self.page - 1).saturating_mul(32))
                    .min(32)
            && self.report.len() <= 129 * 1024
            && self.rows.iter().enumerate().all(|(index, row)| {
                row.label.len() <= 256
                    && row.detail.len() <= 1024
                    && match &row.key {
                        Some(key) => {
                            row.command(&self.kind).is_some()
                                && keys.insert(key)
                                && (self.kind != "forks"
                                    || key.parse::<u64>().ok()
                                        == Some((self.page - 1) * 32 + index as u64 + 1))
                        }
                        None => self.kind == "checkpoints",
                    }
            })
    }
}

/// One page of editable engine-host settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub total: u64,
    pub page: u64,
    pub fields: Vec<EngineConfigField>,
}

/// A setting with an engine-authored address, label, value, and editing hint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfigField {
    pub path: String,
    pub label: String,
    pub value: String,
    pub hint: String,
    pub editable: bool,
}

impl EngineConfigField {
    /// Prepares a single-line setter, preserving spaces inside its value.
    pub fn command(&self, value: &str) -> Option<String> {
        (self.editable
            && self.has_valid_path()
            && value.len() <= 4096
            && !value.chars().any(char::is_control))
        .then(|| format!("/config {} {}", self.path, value.trim()))
    }

    fn has_valid_path(&self) -> bool {
        !self.path.is_empty()
            && self.path.len() <= 256
            && self.path.contains('.')
            && self
                .path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte))
    }
}

impl EngineConfig {
    /// Validates bounded pages and setting addresses before displaying them.
    pub fn is_valid(&self) -> bool {
        let mut paths = std::collections::HashSet::new();
        self.page > 0
            && self.page <= self.total.div_ceil(32).max(1)
            && self.fields.len() as u64
                == self
                    .total
                    .saturating_sub((self.page - 1).saturating_mul(32))
                    .min(32)
            && self.fields.iter().all(|field| {
                field.has_valid_path()
                    && paths.insert(&field.path)
                    && field.label.len() <= 256
                    && field.hint.len() <= 256
                    && field.value.len() <= 4096
            })
    }
}

/// One bounded page of saved conversations on the engine host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSavedSessions {
    pub total: u64,
    pub page: u64,
    pub sessions: Vec<EngineSavedSession>,
}

/// Metadata for a saved engine conversation, independent of workspace tabs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSavedSession {
    pub id: String,
    pub title: String,
    pub tag: String,
    pub cwd: String,
    pub updated: u64,
}

impl EngineSavedSessions {
    /// Validates bounded pages and identifiers used to prepare slash commands.
    pub fn is_valid(&self) -> bool {
        let mut ids = std::collections::HashSet::new();
        self.page > 0
            && self.page <= self.total.div_ceil(64).max(1)
            && self.sessions.len() as u64
                == self
                    .total
                    .saturating_sub((self.page - 1).saturating_mul(64))
                    .min(64)
            && self.sessions.iter().all(|session| {
                !session.id.is_empty()
                    && session.id.len() <= 80
                    && session
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                    && ids.insert(&session.id)
                    && session.title.len() <= 1024
                    && session.tag.len() <= 1024
                    && session.cwd.len() <= 1024
            })
    }
}

/// Engine-owned job snapshots; the UI never owns their processes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineJobs {
    pub running: u64,
    pub total: u64,
    pub jobs: Vec<EngineJob>,
}

/// One background job at the engine's last capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineJob {
    pub id: i64,
    pub pid: u32,
    pub elapsed_secs: f64,
    pub state: String,
    pub exit_code: Option<i64>,
    pub path: String,
}

/// Plank's portable status snapshot, independent of terminal rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineStatus {
    pub state: String,
    pub ctx_used: u64,
    pub ctx_size: u64,
    pub generated: u64,
    pub gen_tps: f64,
    pub elapsed_secs: f64,
    pub think: String,
}

impl EngineTelemetry {
    /// Formats available counters as a compact session status line.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(status) = &self.status {
            parts.push(status.state.clone());
            if status.ctx_size > 0 {
                parts.push(format!("Context {}/{}", status.ctx_used, status.ctx_size));
            }
            if status.gen_tps > 0.0 {
                parts.push(format!("{:.1} tok/s", status.gen_tps));
            }
            if !status.think.is_empty() {
                parts.push(format!("Think {}", status.think));
            }
        }
        if let Some((completed, total)) = self.tasks {
            parts.push(format!("Tasks {completed}/{total}"));
        }
        if let Some(jobs) = &self.jobs
            && jobs.running > 0
        {
            parts.push(format!("Jobs {}", jobs.running));
        }
        parts.join(" · ")
    }
}

/// Commands accepted by the persistent daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Command {
    ConnectRemote {
        session_id: String,
    },
    RemoteControl {
        session_id: String,
        request: bool,
    },
    EditDocument {
        session_id: String,
        approval_id: String,
        requirement: serde_json::Value,
        #[serde(deserialize_with = "required_document_text")]
        text: Option<String>,
    },
    AcknowledgeSession {
        session_id: String,
    },
    RenameWorkspace {
        workspace_id: String,
        name: String,
    },
    ArchiveSession {
        session_id: String,
        archived: bool,
    },
    ReorderSession {
        session_id: String,
        position: i64,
    },
    Resume {
        session_id: String,
    },
    DecideApproval {
        session_id: String,
        approval_id: String,
        choice_id: String,
        requirement: serde_json::Value,
    },
    CreateWorkspace {
        name: String,
        directory: String,
    },
    CreateSession {
        workspace_id: String,
        name: String,
        backend: Backend,
    },
    MoveSession {
        session_id: String,
        workspace_id: String,
    },
    RenameSession {
        session_id: String,
        name: String,
    },
    Prompt {
        session_id: String,
        request_id: String,
        text: String,
    },
    Stop {
        session_id: String,
    },
    Reset {
        session_id: String,
    },
}

fn required_document_text<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// Best-effort host metadata; unavailable ports differ from an empty result.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionContext {
    pub branch: Option<String>,
    pub dirty: bool,
    pub ports: Option<Vec<u16>>,
}

/// Identifies the object affected by an accepted command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn config_preparation_preserves_spaces_and_rejects_extra_commands() {
        let mut field = super::EngineConfigField {
            path: "engine.model".into(),
            label: "Model".into(),
            value: String::new(),
            hint: "text".into(),
            editable: true,
        };
        assert_eq!(
            field.command("/models/My Model.gguf"),
            Some("/config engine.model /models/My Model.gguf".into())
        );
        assert_eq!(field.command(""), Some("/config engine.model ".into()));
        assert!(field.command("value\n/quit").is_none());
        assert!(field.command("value\r/quit").is_none());
        field.path = "engine.model\n/quit".into();
        assert!(field.command("value").is_none());
        field.path = "pluginConfig.timer.delay".into();
        field.editable = false;
        assert!(field.command("10").is_none());
    }

    #[test]
    fn document_commands_require_explicit_save_or_cancel() {
        let mut command = serde_json::json!({"type":"edit_document", "session_id":"s", "approval_id":"e", "requirement":{}});
        assert!(serde_json::from_value::<Command>(command.clone()).is_err());
        command["text"] = serde_json::Value::Null;
        assert!(matches!(
            serde_json::from_value::<Command>(command.clone()).unwrap(),
            Command::EditDocument { text: None, .. }
        ));
        command["text"] = serde_json::json!("");
        assert!(
            matches!(serde_json::from_value::<Command>(command).unwrap(), Command::EditDocument { text: Some(text), .. } if text.is_empty())
        );
    }
}
