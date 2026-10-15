//! Plank command discovery shared by native and browser clients.
//!
//! The bundled engine catalog remains authoritative. Picking a command only
//! prepares the composer; submitting it uses the normal durable request path.

// Rust guideline compliant 2026-02-21
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// A discoverable Plank command and its argument syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlankCommand {
    pub name: String,
    pub args: String,
    pub description: String,
    #[serde(default = "builtin_source")]
    pub source: String,
    #[serde(default)]
    pub requirement: Option<String>,
    /// Declared template variables; absent on older engines and other commands.
    #[serde(default)]
    pub parameters: Option<Vec<String>>,
}

fn builtin_source() -> String {
    "builtin".into()
}

/// Identifies the exact aliases that open graphical command discovery.
pub fn is_picker_command(text: &str) -> bool {
    matches!(text.trim(), "/command" | "/commands")
}

/// Extracts a nonempty side question from an exact `/btw` command.
pub fn aside_question(text: &str) -> Option<&str> {
    let rest = text.trim().strip_prefix("/btw")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let question = rest.trim();
    (!question.is_empty()).then_some(question)
}

impl PlankCommand {
    /// Prepares named template arguments while preserving quotes and backslashes.
    pub fn template_command(
        &self,
        values: &std::collections::HashMap<String, String>,
    ) -> Option<String> {
        if self.source != "template" {
            return None;
        }
        let parameters = self.parameters.as_ref()?;
        if !self.name.starts_with('/')
            || self.name.len() > 256
            || self.name.contains(char::is_whitespace)
            || self.name.contains(char::is_control)
            || parameters.len() > 32
            || parameters.iter().enumerate().any(|(index, name)| {
                name.is_empty()
                    || name.len() > 64
                    || !name
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
                    || parameters[..index].contains(name)
            })
        {
            return None;
        }
        let mut command = self.name.clone();
        for name in parameters {
            let value = values.get(name)?;
            if value.len() > 4096 || value.chars().any(char::is_control) {
                return None;
            }
            command.push_str(&format!(
                " {name}=\"{}\"",
                value.replace('\\', "\\\\").replace('"', "\\\"")
            ));
        }
        (command.len() <= 256 * 1024).then_some(command)
    }
}

/// Prepares a single-line task edit using the engine's argument limit.
pub fn task_edit_command(action: &str, id: Option<u32>, value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    let argument = match (action, id) {
        ("add", None) if !value.is_empty() => format!("add {value}"),
        ("rename", Some(id)) if id > 0 && !value.is_empty() => format!("rename {id} {value}"),
        ("active", Some(id)) if id > 0 => {
            format!("active {id} {}", if value.is_empty() { "-" } else { value })
        }
        _ => return None,
    };
    (argument.len() <= 1024).then(|| format!("/tasks {argument}"))
}

/// Prepares a bounded goal without interpreting objective text as an option.
pub fn goal_command(objective: &str, iterations: &str) -> Option<String> {
    // Match the 64 KiB bound used by independently submitted side questions.
    if objective.len() > 64 * 1024 || objective.chars().any(char::is_control) {
        return None;
    }
    let objective = objective.trim();
    let iterations: usize = iterations.trim().parse().ok()?;
    (!objective.is_empty() && iterations > 0)
        .then(|| format!("/goal --max {iterations} {objective}"))
}

/// Inline operations on the current engine conversation and its saved history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationEdit {
    Rename,
    Tag,
    Checkpoint,
    Search,
    RatePositive,
    RateNegative,
}

impl ConversationEdit {
    /// Returns the engine command used by this operation.
    pub fn command_name(self) -> &'static str {
        match self {
            Self::Rename => "/rename",
            Self::Tag => "/tag",
            Self::Checkpoint => "/checkpoint",
            Self::Search => "/search",
            Self::RatePositive | Self::RateNegative => "/rate",
        }
    }

    /// Prepares one explicit operation without splitting labels into new commands.
    pub fn prepare(self, value: &str, all_projects: bool) -> Option<String> {
        if value.len() > 4096 || value.chars().any(char::is_control) {
            return None;
        }
        let value = value.trim();
        let argument = match self {
            Self::Rename => {
                // Match Plank's NAME_MAX and reserved cache-file prefix.
                if value.is_empty()
                    || value.len() > 64
                    || value.to_ascii_lowercase().starts_with("sysprompt")
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                {
                    return None;
                }
                value.to_owned()
            }
            Self::Tag => {
                if value.is_empty() {
                    "-".into()
                } else {
                    value.into()
                }
            }
            Self::Checkpoint => {
                if value.is_empty()
                    || value
                        .strip_prefix("--page ")
                        .is_some_and(|page| page.parse::<u64>().is_ok())
                {
                    return None;
                }
                value.to_owned()
            }
            Self::Search => {
                if value.is_empty()
                    || (!all_projects
                        && value.strip_suffix("--all").is_some_and(|prefix| {
                            prefix.ends_with(char::is_whitespace) && !prefix.trim().is_empty()
                        }))
                {
                    return None;
                }
                format!("{value}{}", if all_projects { " --all" } else { "" })
            }
            Self::RatePositive | Self::RateNegative => {
                let rating = if self == Self::RatePositive { "+" } else { "-" };
                format!(
                    "{rating}{}",
                    if value.is_empty() {
                        String::new()
                    } else {
                        format!(" {value}")
                    }
                )
            }
        };
        Some(format!("{} {argument}", self.command_name()))
    }
}

/// Prepares a bounded component settings page without treating an ID as options.
pub fn component_config_command(component: &str, page: u64) -> Option<String> {
    (!component.is_empty()
        && component.len() <= 128
        && page > 0
        && component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-:/".contains(&byte)))
    .then(|| format!("/config --component {component} --page {page}"))
}

/// The command capabilities reported by a live Plank engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineControls {
    pub version: u32,
    pub commands: Vec<PlankCommand>,
    #[serde(default)]
    pub settings: Option<EngineSettings>,
    #[serde(default)]
    pub features: Vec<EngineFeature>,
    /// Current engine-authored draft hint; never submitted without user acceptance.
    #[serde(default)]
    pub suggestion: Option<PromptSuggestion>,
}

/// A bounded plain-text suggestion tied to one engine offer.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptSuggestion {
    pub revision: String,
    pub text: String,
}

impl std::fmt::Debug for PromptSuggestion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PromptSuggestion")
            .field("revision", &self.revision)
            .field("characters", &self.text.chars().count())
            .finish()
    }
}

impl PromptSuggestion {
    /// Rejects oversized offers, control characters, and executable command prefixes.
    pub fn is_valid(&self) -> bool {
        !self.revision.is_empty()
            && self.revision.len() <= 128
            && self
                .revision
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
            && !self.text.is_empty()
            && self.text.trim() == self.text
            && self.text.chars().count() <= 120
            && !self.text.chars().any(char::is_control)
            && !self.text.starts_with(['/', '!'])
    }
}

/// A build feature or loaded engine capability, distinct from command availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineFeature {
    pub name: String,
    pub label: String,
    pub available: bool,
    pub detail: String,
}

/// Engine-reported settings captured alongside its command catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineSettings {
    pub model: String,
    pub temperature: f64,
    pub power_percent: u32,
    pub speculative: bool,
    pub speculation_available: bool,
    pub micro_compaction: bool,
    pub loop_guard: bool,
    /// Requested hint policy; absent on older hosts.
    #[serde(default)]
    pub hints: Option<bool>,
    /// Session delivery policy; absent on older hosts.
    #[serde(default)]
    pub notify_mode: Option<crate::livecontrol::NotifyMode>,
    pub steering: Option<f64>,
}

impl EngineSettings {
    /// Checks settings received from an engine before publishing them.
    pub fn is_valid(&self) -> bool {
        self.model.len() <= 4096
            && !self.model.chars().any(char::is_control)
            && self.temperature.is_finite()
            && (0.0..=100.0).contains(&self.temperature)
            && self.power_percent <= 100
            && self.steering.is_none_or(f64::is_finite)
            && (!self.speculative || self.speculation_available)
    }
}

/// Returns the negotiated Save limit without increasing legacy inline payloads.
pub(crate) fn editor_save_limit(controls: Option<&EngineControls>) -> usize {
    if controls.is_some_and(|controls| {
        controls
            .features
            .iter()
            .any(|feature| feature.name == "editor_stream_save" && feature.available)
    }) {
        crate::documenttransfer::DOCUMENT_BYTES
    } else {
        // Legacy editors accept a single inline reply rather than bounded chunks.
        256 * 1024
    }
}

/// Returns the bundled Plank command catalog.
pub fn catalog() -> &'static [PlankCommand] {
    static CATALOG: OnceLock<Vec<PlankCommand>> = OnceLock::new();
    CATALOG.get_or_init(|| parse_catalog(include_str!("../plank/src/config.rs")))
}

fn parse_catalog(source: &str) -> Vec<PlankCommand> {
    let Some((_, source)) = source.split_once("pub const SLASH_COMMANDS:") else {
        return Vec::new();
    };
    let source = source
        .split_once("\n];")
        .map_or(source, |(catalog, _)| catalog);
    let field = |block: &str, name: &str| -> Option<String> {
        let (_, value) = block.split_once(&format!("{name}: "))?;
        let value = value.lines().next()?.trim().trim_end_matches(',');
        serde_json::from_str(value).ok()
    };
    source
        .split("SlashCommand {")
        .skip(1)
        .filter_map(|block| {
            Some(PlankCommand {
                name: field(block, "name")?,
                args: field(block, "args")?,
                description: field(block, "desc")?,
                source: builtin_source(),
                requirement: None,
                parameters: None,
            })
        })
        .collect()
}

/// Finds commands by name, arguments, or description without changing the input.
pub fn matching(query: &str) -> Vec<&'static PlankCommand> {
    matching_in(catalog(), query)
}

/// Finds matching commands in a session's authoritative runtime catalog.
pub fn matching_in<'a>(commands: &'a [PlankCommand], query: &str) -> Vec<&'a PlankCommand> {
    let query = query.trim().trim_start_matches('/').to_lowercase();
    commands
        .iter()
        .filter(|command| {
            query.is_empty()
                || command.name.to_lowercase().contains(&query)
                || command.description.to_lowercase().contains(&query)
                || command.args.to_lowercase().contains(&query)
                || command
                    .requirement
                    .as_deref()
                    .is_some_and(|requirement| requirement.to_lowercase().contains(&query))
                || command.parameters.as_ref().is_some_and(|parameters| {
                    parameters
                        .iter()
                        .any(|name| name.to_lowercase().contains(&query))
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_picker_aliases_require_exact_commands_and_preserve_other_input() {
        for text in ["/command", "/commands", "  /command \n", "/commands\t"] {
            assert!(super::is_picker_command(text), "{text:?}");
        }
        for text in [
            "command",
            "/command:custom",
            "/commands extra",
            "/command\n/quit",
            "please run /command",
            "/commanded",
        ] {
            assert!(!super::is_picker_command(text), "{text:?}");
        }
    }

    #[test]
    fn conversation_forms_preserve_scope_and_refuse_ambiguous_or_invalid_names() {
        use super::ConversationEdit as Edit;
        assert_eq!(
            Edit::Rename.prepare(" new-name ", false),
            Some("/rename new-name".into())
        );
        for name in ["", "../file", "two names", "SySpRoMpT-cache", "café"] {
            assert!(Edit::Rename.prepare(name, false).is_none());
        }
        assert!(Edit::Rename.prepare(&"a".repeat(64), false).is_some());
        assert!(Edit::Rename.prepare(&"a".repeat(65), false).is_none());
        assert_eq!(Edit::Tag.prepare("", false), Some("/tag -".into()));
        assert_eq!(
            Edit::Tag.prepare("Ready for review", false),
            Some("/tag Ready for review".into())
        );
        assert_eq!(
            Edit::Checkpoint.prepare("Before refactor", false),
            Some("/checkpoint Before refactor".into())
        );
        assert!(Edit::Checkpoint.prepare("--page 2", false).is_none());
        assert!(
            Edit::Checkpoint
                .prepare("--page 4294967296", false)
                .is_none()
        );
        assert_eq!(
            Edit::Search.prepare("two words", true),
            Some("/search two words --all".into())
        );
        assert!(Edit::Search.prepare("query --all", false).is_none());
        assert_eq!(
            Edit::Search.prepare("literal--all", false),
            Some("/search literal--all".into())
        );
        assert_eq!(
            Edit::RatePositive.prepare("", false),
            Some("/rate +".into())
        );
        assert_eq!(
            Edit::RateNegative.prepare("Missed the test", false),
            Some("/rate - Missed the test".into())
        );
        for edit in [
            Edit::Rename,
            Edit::Tag,
            Edit::Checkpoint,
            Edit::Search,
            Edit::RatePositive,
        ] {
            assert!(edit.prepare("one\n/quit", false).is_none());
        }
    }

    #[test]
    fn task_forms_preserve_labels_and_refuse_control_characters_and_invalid_ids() {
        use super::task_edit_command as command;
        assert_eq!(
            command("add", None, "  Review café /quit  "),
            Some("/tasks add Review café /quit".into())
        );
        assert_eq!(
            command("rename", Some(7), "A subject with spaces"),
            Some("/tasks rename 7 A subject with spaces".into())
        );
        assert_eq!(
            command("active", Some(7), ""),
            Some("/tasks active 7 -".into())
        );
        for (action, id, text) in [
            ("rename", Some(0), "bad"),
            ("add", None, ""),
            ("add", Some(1), "bad"),
            ("delete", Some(1), "bad"),
            ("add", None, "one\n/quit"),
            ("active", Some(1), "one\r/quit"),
        ] {
            assert!(command(action, id, text).is_none());
        }
        assert!(command("add", None, &"é".repeat(510)).is_some());
        assert!(command("add", None, &"é".repeat(511)).is_none());
    }

    #[test]
    fn goal_forms_keep_reserved_objectives_as_text_and_require_a_positive_cap() {
        use super::goal_command as command;
        assert_eq!(command("clear", "20"), Some("/goal --max 20 clear".into()));
        assert_eq!(
            command("--max 2 ship it", "3"),
            Some("/goal --max 3 --max 2 ship it".into())
        );
        assert_eq!(
            command("  Ship it  ", " 5 "),
            Some("/goal --max 5 Ship it".into())
        );
        for (goal, count) in [
            ("", "20"),
            ("work", "0"),
            ("work", "-1"),
            ("work", "twenty"),
            ("one\n/quit", "20"),
        ] {
            assert!(command(goal, count).is_none());
        }
    }
    #[test]
    fn prompt_suggestions_validate_bounded_prose_and_hide_text_in_debug() {
        let suggestion = super::PromptSuggestion {
            revision: "offer-1".into(),
            text: "Write tests for the parser".into(),
        };
        assert!(suggestion.is_valid());
        assert!(!format!("{suggestion:?}").contains("parser"));
        for text in ["/clear", "!run", " leading", "trailing ", "line\nbreak", ""] {
            assert!(
                !super::PromptSuggestion {
                    text: text.into(),
                    ..suggestion.clone()
                }
                .is_valid()
            );
        }
        assert!(
            super::PromptSuggestion {
                text: "🦀".repeat(120),
                ..suggestion.clone()
            }
            .is_valid()
        );
        assert!(
            !super::PromptSuggestion {
                text: "🦀".repeat(121),
                ..suggestion
            }
            .is_valid()
        );
    }

    use super::*;

    #[test]
    fn side_questions_require_the_exact_command_and_keep_multiline_arguments() {
        assert_eq!(
            aside_question(" /btw  Why?\nExplain 🦀. "),
            Some("Why?\nExplain 🦀.")
        );
        assert_eq!(aside_question("/btw\twhy"), Some("why"));
        for text in ["/btw", "/btw ", "/btwfoo why", "btw why", "prompt /btw why"] {
            assert_eq!(aside_question(text), None);
        }
    }

    #[test]
    fn template_forms_quote_values_and_reject_parameter_or_command_injection() {
        let mut command = PlankCommand {
            name: "/review".into(),
            args: "<path> <text> <blank>".into(),
            description: "Review".into(),
            source: "template".into(),
            requirement: None,
            parameters: Some(vec!["path".into(), "text".into(), "blank".into()]),
        };
        let values = std::collections::HashMap::from([
            ("path".into(), r"C:\repo\file.rs".into()),
            ("text".into(), "say \"hello\"".into()),
            ("blank".into(), String::new()),
        ]);
        assert_eq!(
            command.template_command(&values).as_deref(),
            Some(r#"/review path="C:\\repo\\file.rs" text="say \"hello\"" blank="""#)
        );
        let mut invalid = values.clone();
        invalid.insert("text".into(), "line\n/quit".into());
        assert!(command.template_command(&invalid).is_none());
        invalid.insert("text".into(), "x".repeat(4097));
        assert!(command.template_command(&invalid).is_none());
        command.parameters = Some(vec!["bad=value".into()]);
        assert!(command.template_command(&values).is_none());
        command.parameters = None;
        assert!(command.template_command(&values).is_none());
    }

    #[test]
    fn editor_save_limits_require_an_available_streaming_capability() {
        assert_eq!(super::editor_save_limit(None), 256 * 1024);
        for (available, expected) in [
            (false, 256 * 1024),
            (true, crate::documenttransfer::DOCUMENT_BYTES),
        ] {
            let controls = serde_json::from_value(serde_json::json!({
                "version":1,"commands":[],"features":[{
                    "name":"editor_stream_save","label":"Chunked Saves",
                    "available":available,"detail":"Fixture"
                }]
            }))
            .unwrap();
            assert_eq!(super::editor_save_limit(Some(&controls)), expected);
        }
    }

    #[test]
    fn catalog_preserves_every_upstream_entry_and_argument() {
        let source = include_str!("../plank/src/config.rs");
        let section = source
            .split_once("pub const SLASH_COMMANDS:")
            .unwrap()
            .1
            .split_once("\n];")
            .unwrap()
            .0;
        assert_eq!(catalog().len(), section.matches("SlashCommand {").count());
        assert!(catalog().len() > 60);
        let goal = catalog()
            .iter()
            .find(|command| command.name == "/goal")
            .unwrap();
        assert_eq!(goal.args, "[--max n] <objective>");
        assert!(
            catalog()
                .iter()
                .all(|command| !command.description.is_empty())
        );
    }

    #[test]
    fn filtering_supports_slash_prefixes_and_descriptions() {
        assert!(
            matching("/compact")
                .iter()
                .any(|command| command.name == "/compact")
        );
        assert!(matching("MCP").iter().any(|command| command.name == "/mcp"));
        assert!(
            matching("checkpoint")
                .iter()
                .any(|command| command.name == "/rollback")
        );
        assert!(matching("no-such-control-8291").is_empty());
    }

    #[test]
    fn runtime_requirements_are_searchable_and_older_catalogs_still_load() {
        let old = serde_json::json!({"version": 1, "commands": [
            {"name": "/frame", "args": "[id]", "description": "Open a frame"}
        ]});
        let mut controls: EngineControls = serde_json::from_value(old).unwrap();
        assert!(controls.features.is_empty());
        assert!(controls.commands[0].requirement.is_none());
        controls.commands[0].requirement = Some("Requires the terminal UI".into());
        assert_eq!(matching_in(&controls.commands, "terminal").len(), 1);
        assert_eq!(matching_in(&controls.commands, "/frame").len(), 1);
        let strip = catalog()
            .iter()
            .find(|command| command.name == "/strip")
            .unwrap();
        assert_eq!(strip.args, "<prefix>");
        assert!(strip.description.contains("saved session"));
    }
}
