//! One registry drives keyboard dispatch, configurable presets, and command discovery.

// Rust guideline compliant 2026-02-21
use gpui::{Action, KeyBinding};
use gpui_keybinding::{
    DocumentedKeybinding, KeybindingCategory, KeybindingProvider, KeybindingRegistry, KeymapPreset,
    apply_user_overrides, format_key_label,
};
use serde::Deserialize;

/// Dispatches a documented application command in the focused window.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub(crate) struct ExecuteCommand {
    pub name: String,
}

impl Action for ExecuteCommand {
    fn boxed_clone(&self) -> Box<dyn Action> {
        Box::new(self.clone())
    }
    fn partial_eq(&self, action: &dyn Action) -> bool {
        action.as_any().downcast_ref::<Self>() == Some(self)
    }
    fn name(&self) -> &'static str {
        Self::name_for_type()
    }
    fn name_for_type() -> &'static str {
        "nplank::ExecuteCommand"
    }
    fn build(value: serde_json::Value) -> anyhow::Result<Box<dyn Action>> {
        Ok(Box::new(serde_json::from_value::<Self>(value)?))
    }
}

pub(crate) struct ShortcutRegistry(KeybindingRegistry);
impl std::fmt::Debug for ShortcutRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShortcutRegistry").finish_non_exhaustive()
    }
}
impl std::ops::Deref for ShortcutRegistry {
    type Target = KeybindingRegistry;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
struct Bindings {
    scope: String,
    docs: Vec<DocumentedKeybinding>,
}

fn context(scope: &str, binding: &DocumentedKeybinding) -> String {
    binding.context.as_ref().map_or_else(
        || scope.to_owned(),
        |context| format!("{scope} && {context}"),
    )
}

impl KeybindingProvider for Bindings {
    fn bindings(&self, _: KeymapPreset) -> Vec<KeyBinding> {
        self.docs
            .iter()
            .filter_map(|binding| {
                let spec = binding.raw_key_spec.as_deref()?;
                if spec.is_empty() {
                    return None;
                }
                Some(KeyBinding::new(
                    spec,
                    ExecuteCommand {
                        name: binding.description.clone(),
                    },
                    Some(&context(&self.scope, binding)),
                ))
            })
            .collect()
    }

    fn documented_bindings(&self, _: KeymapPreset) -> Vec<DocumentedKeybinding> {
        self.docs.clone()
    }
}

fn defaults(preset: KeymapPreset) -> Vec<DocumentedKeybinding> {
    let primary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    let mut docs = Vec::new();
    for (name, suffix, category) in [
        ("Command palette", "shift-p", KeybindingCategory::Search),
        ("Toggle navigator", "b", KeybindingCategory::View),
        ("Split right", "alt-v", KeybindingCategory::View),
        ("Split down", "alt-h", KeybindingCategory::View),
        ("Close pane", "alt-w", KeybindingCategory::View),
        ("Focus composer", "l", KeybindingCategory::Navigation),
        (
            "Focus pending question",
            "shift-u",
            KeybindingCategory::Navigation,
        ),
        (
            "Focus transcript",
            "shift-l",
            KeybindingCategory::Navigation,
        ),
        ("Next session", "alt-right", KeybindingCategory::Navigation),
        (
            "Previous session",
            "alt-left",
            KeybindingCategory::Navigation,
        ),
        ("Next worktree", "alt-down", KeybindingCategory::Navigation),
        (
            "Previous worktree",
            "alt-up",
            KeybindingCategory::Navigation,
        ),
        (
            "Next project",
            "alt-shift-down",
            KeybindingCategory::Navigation,
        ),
        (
            "Previous project",
            "alt-shift-up",
            KeybindingCategory::Navigation,
        ),
        (
            "Next needs attention",
            "shift-j",
            KeybindingCategory::Navigation,
        ),
        ("New agent", "n", KeybindingCategory::FileOps),
        ("New workspace", "shift-n", KeybindingCategory::FileOps),
        ("Preferences", ",", KeybindingCategory::System),
        (
            "Projects and worktrees",
            "shift-w",
            KeybindingCategory::FileOps,
        ),
        ("Engine commands", "/", KeybindingCategory::Search),
        ("Toggle details", "shift-e", KeybindingCategory::View),
        ("Toggle diagnostics", "alt-d", KeybindingCategory::View),
        ("Toggle fleet", "shift-f", KeybindingCategory::View),
        ("Toggle CI", "shift-i", KeybindingCategory::View),
        ("Toggle docs", "shift-d", KeybindingCategory::View),
        ("Toggle monitor", "shift-g", KeybindingCategory::View),
        ("Paste image", "shift-v", KeybindingCategory::Editing),
    ] {
        let spec = format!("{primary}-{suffix}");
        docs.push(
            DocumentedKeybinding::new(format_key_label(&spec), name, category)
                .with_raw_key_spec(spec),
        );
    }
    let (previous, next) = match preset {
        KeymapPreset::Vim => ("k", "j"),
        KeymapPreset::Emacs => ("ctrl-p", "ctrl-n"),
        _ => ("up", "down"),
    };
    for (name, spec, scope) in [
        ("Navigator previous", previous, "Navigator"),
        ("Navigator next", next, "Navigator"),
        ("Open selected conversation", "enter", "Navigator"),
        ("Transcript page up", "pageup", "Transcript"),
        ("Transcript page down", "pagedown", "Transcript"),
        ("Transcript start", "home", "Transcript"),
        ("Transcript end", "end", "Transcript"),
    ] {
        docs.push(
            DocumentedKeybinding::new(format_key_label(spec), name, KeybindingCategory::Navigation)
                .with_raw_key_spec(spec)
                .with_context(scope),
        );
    }
    for name in [
        "Keyboard shortcuts",
        "Show archived sessions",
        "Close session tab",
        "Reopen closed tabs",
    ] {
        docs.push(
            DocumentedKeybinding::new("Unbound", name, KeybindingCategory::View)
                .with_raw_key_spec(""),
        );
    }
    docs
}

/// Validates overrides before installing any keys, preserving the old map on failure.
pub(crate) fn registry(
    scope: &str,
    preset: KeymapPreset,
    overrides: Vec<DocumentedKeybinding>,
) -> anyhow::Result<ShortcutRegistry> {
    let base = defaults(preset);
    for binding in &overrides {
        let original = base
            .iter()
            .find(|entry| entry.description == binding.description)
            .ok_or_else(|| anyhow::anyhow!("Unknown command: {}", binding.description))?;
        anyhow::ensure!(
            original.context == binding.context,
            "Keep the focus context for {}",
            binding.description
        );
    }
    let mut docs = apply_user_overrides(base, overrides);
    for binding in &mut docs {
        let spec = binding
            .raw_key_spec
            .as_deref()
            .unwrap_or(&binding.key)
            .to_owned();
        // An empty spec deliberately disables a shortcut while retaining its palette action.
        for stroke in spec.split_whitespace() {
            let parsed = gpui::Keystroke::parse(stroke).map_err(|error| {
                anyhow::anyhow!("Invalid shortcut for {}: {error}", binding.description)
            })?;
            anyhow::ensure!(
                binding.context.is_some()
                    || parsed.modifiers.platform
                    || parsed.modifiers.control
                    || parsed.modifiers.alt,
                "Global shortcuts need a modifier: {}",
                binding.description
            );
        }
        binding.key = if spec.is_empty() {
            "Unbound".into()
        } else {
            format_key_label(&spec).into_owned()
        };
        binding.raw_key_spec = Some(spec);
    }
    let registry = KeybindingRegistry::new();
    let conflicts = gpui_keybinding::detect_conflicts(
        &docs
            .iter()
            .filter(|b| b.raw_key_spec.as_deref() != Some(""))
            .cloned()
            .collect::<Vec<_>>(),
    );
    anyhow::ensure!(conflicts.is_empty(), "Conflicting shortcuts: {conflicts:?}");
    let mut registry = registry;
    registry.register(Bindings {
        scope: scope.into(),
        docs,
    });
    Ok(ShortcutRegistry(registry))
}

/// Replaces only this view's bindings, leaving editor and other window maps intact.
pub(crate) fn install(
    scope: &str,
    previous: Option<&ShortcutRegistry>,
    next: &ShortcutRegistry,
    preset: KeymapPreset,
    cx: &mut gpui::App,
) {
    if let Some(previous) = previous {
        for binding in previous.get_documented(preset).iter() {
            if let Some(spec) = binding.raw_key_spec.as_deref().filter(|s| !s.is_empty()) {
                cx.bind_keys([KeyBinding::new(
                    spec,
                    gpui::NoAction,
                    Some(&context(scope, binding)),
                )]);
            }
        }
    }
    cx.bind_keys(next.get_bindings(preset).iter().cloned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_keep_typing_safe_and_have_no_conflicts() {
        for preset in KeymapPreset::all() {
            let registry = registry("TestView", *preset, vec![]).unwrap();
            let bound = registry
                .get_documented(*preset)
                .iter()
                .filter(|binding| binding.raw_key_spec.as_deref() != Some(""))
                .cloned()
                .collect::<Vec<_>>();
            assert!(gpui_keybinding::detect_conflicts(&bound).is_empty());
            assert_eq!(registry.get_bindings(*preset).len(), 34);
        }
    }

    #[test]
    fn remaps_replace_defaults_and_reject_conflicts_and_unknown_actions() {
        let mut binding = defaults(KeymapPreset::Default).remove(1);
        binding.raw_key_spec = Some("ctrl-alt-b".into());
        let mapped = registry("TestView", KeymapPreset::Default, vec![binding.clone()]).unwrap();
        assert_eq!(
            mapped.get_documented(KeymapPreset::Default)[1]
                .raw_key_spec
                .as_deref(),
            Some("ctrl-alt-b")
        );
        binding.raw_key_spec = defaults(KeymapPreset::Default)[0].raw_key_spec.clone();
        assert!(registry("TestView", KeymapPreset::Default, vec![binding.clone()]).is_err());
        binding.raw_key_spec = Some("b".into());
        assert!(registry("TestView", KeymapPreset::Default, vec![binding.clone()]).is_err());
        binding.description = "Unknown action".into();
        assert!(registry("TestView", KeymapPreset::Default, vec![binding]).is_err());
    }
}
