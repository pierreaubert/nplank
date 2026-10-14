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

/// The command capabilities reported by a live Plank engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineControls {
    pub version: u32,
    pub commands: Vec<PlankCommand>,
    #[serde(default)]
    pub settings: Option<EngineSettings>,
    #[serde(default)]
    pub features: Vec<EngineFeature>,
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
    use super::*;

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
