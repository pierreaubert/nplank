//! Native workspace views composed with toolkit macros and semantic design tokens.

// Rust guideline compliant 2026-02-21
use crate::{
    client::Client,
    model::{AgentProvider, Backend, Command, Entry, Session, Snapshot},
};
use gpui::{App, Context, SharedString, Window, div, prelude::*, px};
use gpui_design::{DesignExt, DesignSystem, DesignSystemState};
use gpui_ui_kit::theme::{ThemeState, ThemeVariant};
use gpui_ui_kit::{
    Button, ButtonSize, ButtonVariant, Input, Markdown, Progress, ProgressSize, ThemeExt,
};
use gpui_ui_kit_macros::{ComponentTheme, layout};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};

gpui::actions!(
    nplank,
    [
        NewAgent,
        NewWorkspace,
        OpenPreferences,
        ToggleArchived,
        OpenCommands
    ]
);

/// A native appearance menu selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectAppearance {
    pub theme: Option<usize>,
    pub design: Option<usize>,
}
impl gpui::Action for SelectAppearance {
    fn boxed_clone(&self) -> Box<dyn gpui::Action> {
        Box::new(*self)
    }
    fn partial_eq(&self, action: &dyn gpui::Action) -> bool {
        action.as_any().downcast_ref::<Self>() == Some(self)
    }
    fn name(&self) -> &'static str {
        Self::name_for_type()
    }
    fn name_for_type() -> &'static str {
        "nplank::SelectAppearance"
    }
    fn build(_: serde_json::Value) -> anyhow::Result<Box<dyn gpui::Action>> {
        anyhow::bail!("Appearance selections are constructed by menus")
    }
}

#[derive(Debug, Clone, ComponentTheme)]
struct WorkspaceTheme {
    #[theme(default = 0x111827, from = background)]
    background: gpui::Rgba,
    #[theme(default = 0x1f2937, from = surface)]
    surface: gpui::Rgba,
    #[theme(default = 0xe5e7eb, from = text_primary)]
    text: gpui::Rgba,
    #[theme(default = 0x9ca3af, from = text_secondary)]
    muted: gpui::Rgba,
    #[theme(default = 0x3b82f6, from = accent)]
    accent: gpui::Rgba,
}

#[derive(Debug)]
enum Operation {
    Select(Option<String>, i64),
    Command(Command),
    Download(ExportDownload),
}

#[derive(Debug)]
struct ExportDownload {
    key: String,
    session: String,
    generation: u64,
    export: crate::model::EngineExport,
    #[cfg(not(target_family = "wasm"))]
    path: PathBuf,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum ActivityTab {
    #[default]
    Tasks,
    Jobs,
    Agents,
    Model,
    Sessions,
    Config,
    History,
    Extensions,
    Cache,
}

#[derive(Debug)]
enum Update {
    Snapshot(Option<String>, Snapshot),
    Selected(String),
    WorkspaceCreated(String),
    Error(String),
    Accepted(Option<(String, String)>),
    ExportFinished(String, std::result::Result<String, String>),
    #[cfg(target_family = "wasm")]
    ExportReady(String, String, String),
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Preferences {
    #[serde(default)]
    documents: HashMap<String, crate::document::Document>,
    theme: usize,
    design: usize,
    selected: Option<String>,
    #[serde(default)]
    drafts: HashMap<String, String>,
    #[serde(default)]
    collapsed: HashSet<String>,
    #[serde(default)]
    pending_prompts: HashMap<String, (String, String)>,
}

/// A responsive workspace shell with a background-only network bridge.
#[derive(Debug)]
pub struct WorkspaceView {
    composer_focus: gpui::FocusHandle,
    command_focus: gpui::FocusHandle,
    document_search_focus: gpui::FocusHandle,
    command_scroll: gpui::ScrollHandle,
    command_cursor: usize,
    command_dismissed: bool,
    focus_commands: bool,
    documents: HashMap<String, crate::document::Document>,
    #[cfg(target_os = "macos")]
    utilization: crate::utilization::Monitor,
    #[cfg(target_os = "macos")]
    meters: crate::utilization::Utilization,
    snapshot: Snapshot,
    selected: Option<String>,
    workspace: Option<String>,
    drafts: HashMap<String, String>,
    pending_prompts: HashMap<String, (String, String)>,
    collapsed: HashSet<String>,
    fields: HashMap<String, String>,
    tx: mpsc::Sender<Operation>,
    rx: mpsc::Receiver<Update>,
    error: Option<String>,
    connected: bool,
    export_jobs: HashSet<String>,
    export_feedback: Option<(String, String)>,
    #[cfg(target_family = "wasm")]
    browser_exports: HashMap<String, (String, String)>,
    show_new: bool,
    appearance: bool,
    appearance_only: bool,
    agent_choice: usize,
    advanced: bool,
    show_archived: bool,
    details: bool,
    show_tasks: bool,
    activity_tab: ActivityTab,
    history_checkpoints: bool,
    history_tree: bool,
    task_full_report: bool,
    task_edit_id: Option<u32>,
    saved_current: bool,
    extension_kind: &'static str,
    extension_template: Option<String>,
    model_view: &'static str,
    metrics_kind: &'static str,
    show_commands: bool,
    command_query: String,
    expanded: HashSet<i64>,
    draft_dirty: bool,
    preferences: Preferences,
    #[cfg(not(target_family = "wasm"))]
    directory: PathBuf,
    before: i64,
    mobile_sidebar: bool,
    #[cfg(not(target_family = "wasm"))]
    preferences_modified: Option<std::time::SystemTime>,
    #[cfg(target_family = "wasm")]
    preferences_saved: Option<String>,
}

#[cfg(target_family = "wasm")]
impl Drop for WorkspaceView {
    fn drop(&mut self) {
        for (_, (url, _)) in self.browser_exports.drain() {
            let _ = web_sys::Url::revoke_object_url(&url);
        }
    }
}

impl WorkspaceView {
    /// Opens the compact Plank command picker for the selected session.
    pub fn open_commands(&mut self, cx: &mut Context<Self>) {
        if !self.snapshot.sessions.iter().any(|session| {
            Some(&session.id) == self.selected.as_ref()
                && matches!(
                    session.backend,
                    Backend::Local { .. } | Backend::Remote { .. }
                )
        }) {
            return;
        }
        self.show_commands = !self.show_commands;
        self.appearance = false;
        self.details = false;
        self.show_new = false;
        self.command_query.clear();
        self.command_cursor = 0;
        self.command_dismissed = false;
        self.focus_commands = self.show_commands;
        cx.notify();
    }

    fn prepare_command(&mut self, name: &str, has_args: bool, cx: &mut Context<Self>) {
        if self.controls().iter().any(|command| {
            command.name == name
                && command.source == "template"
                && command
                    .parameters
                    .as_ref()
                    .is_some_and(|parameters| !parameters.is_empty())
        }) {
            self.extension_kind = "templates";
            self.extension_template = Some(name.to_owned());
            self.activity_tab = ActivityTab::Extensions;
            self.show_tasks = true;
            self.show_commands = false;
            self.command_dismissed = true;
            cx.notify();
            return;
        }
        if let Some(id) = &self.selected {
            self.drafts.insert(
                id.clone(),
                format!("{name}{}", if has_args { " " } else { "" }),
            );
            self.draft_dirty = true;
        }
        self.show_commands = false;
        self.command_dismissed = true;
        cx.notify();
    }

    /// Opens agent creation for a chosen or currently selected workspace.
    pub fn add_agent(&mut self, workspace: Option<String>, cx: &mut Context<Self>) {
        if self.appearance_only {
            return;
        }
        self.workspace = workspace
            .or_else(|| self.workspace.clone())
            .or_else(|| self.snapshot.workspaces.first().map(|w| w.id.clone()));
        self.agent_choice = 1;
        self.advanced = false;
        self.show_new = true;
        self.appearance = false;
        cx.notify();
    }

    /// Opens the preferences pane.
    pub fn open_preferences(&mut self, cx: &mut Context<Self>) {
        self.appearance = true;
        self.show_new = false;
        cx.notify();
    }

    /// Applies and persists a theme or design selected from native menus.
    pub fn select_appearance(&mut self, selection: &SelectAppearance, cx: &mut Context<Self>) {
        if let Some(theme) = selection.theme {
            self.preferences.theme = theme % ThemeVariant::all().len();
        }
        if let Some(design) = selection.design {
            self.preferences.design = design % 7;
        }
        self.apply_appearance(cx);
        self.save_preferences();
        cx.notify();
    }

    /// Opens workspace creation using the agent dialog.
    pub fn add_workspace(&mut self, cx: &mut Context<Self>) {
        self.add_agent(None, cx);
        self.advanced = true;
    }

    /// Toggles visibility of archived sessions.
    pub fn toggle_archived(&mut self, cx: &mut Context<Self>) {
        self.show_archived = !self.show_archived;
        cx.notify();
    }
    /// Creates a view and starts its independent network bridge.
    pub fn new(
        client: Client,
        directory: PathBuf,
        appearance: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let preferences = load_preferences(&directory);
        let (tx, rx) = if appearance {
            let (tx, _) = mpsc::channel();
            let (_, rx) = mpsc::channel();
            (tx, rx)
        } else {
            bridge(client, preferences.selected.clone())
        };
        let mut fields = HashMap::new();
        fields.insert("workspace-name".into(), "My workspace".into());
        fields.insert(
            "directory".into(),
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        );
        fields.insert("session-name".into(), "New session".into());
        fields.insert("executable".into(), "plank".into());
        fields.insert("arguments".into(), "[]".into());
        fields.insert("agent-arguments".into(), "[]".into());
        fields.insert("remote-url".into(), "ws://127.0.0.1:9000/".into());
        fields.insert("remote-token-env".into(), "PLANK_REMOTE_TOKEN".into());
        cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if entity
                    .update(cx, |view, cx| {
                        view.poll(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let view = Self {
            composer_focus: cx.focus_handle(),
            command_focus: cx.focus_handle(),
            document_search_focus: cx.focus_handle(),
            command_scroll: gpui::ScrollHandle::new(),
            command_cursor: 0,
            command_dismissed: false,
            focus_commands: false,
            #[cfg(target_os = "macos")]
            utilization: crate::utilization::Monitor::new(),
            #[cfg(target_os = "macos")]
            meters: crate::utilization::Utilization::default(),
            snapshot: Snapshot::default(),
            selected: preferences.selected.clone(),
            workspace: None,
            drafts: preferences.drafts.clone(),
            documents: preferences.documents.clone(),
            pending_prompts: preferences.pending_prompts.clone(),
            collapsed: preferences.collapsed.clone(),
            fields,
            tx,
            rx,
            error: None,
            connected: false,
            export_jobs: HashSet::new(),
            export_feedback: None,
            #[cfg(target_family = "wasm")]
            browser_exports: HashMap::new(),
            show_new: false,
            appearance,
            appearance_only: appearance,
            agent_choice: 1,
            advanced: false,
            show_archived: false,
            details: false,
            show_tasks: false,
            activity_tab: ActivityTab::Tasks,
            history_checkpoints: true,
            history_tree: false,
            task_full_report: false,
            task_edit_id: None,
            extension_kind: "plugins",
            extension_template: None,
            model_view: "settings",
            saved_current: false,
            metrics_kind: "context",
            show_commands: false,
            command_query: String::new(),
            expanded: HashSet::new(),
            draft_dirty: false,
            preferences,
            #[cfg(not(target_family = "wasm"))]
            directory,
            before: 0,
            mobile_sidebar: false,
            #[cfg(not(target_family = "wasm"))]
            preferences_modified: None,
            #[cfg(target_family = "wasm")]
            preferences_saved: None,
        };
        view.apply_appearance(cx);
        view
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        #[cfg(target_os = "macos")]
        {
            let sample = self.utilization.sample();
            changed |= self.meters != sample;
            self.meters = sample;
        }
        if self.draft_dirty {
            self.save_preferences();
            self.draft_dirty = false;
        }
        #[cfg(target_family = "wasm")]
        {
            let saved = browser_preferences();
            if saved != self.preferences_saved {
                if let Some(saved) = &saved
                    && let Ok(preferences) = serde_json::from_str::<Preferences>(saved)
                {
                    self.preferences.theme = preferences.theme;
                    self.preferences.design = preferences.design;
                    self.apply_appearance(cx);
                    changed = true;
                }
                self.preferences_saved = saved;
            }
        }
        #[cfg(not(target_family = "wasm"))]
        {
            let modified = std::fs::metadata(self.directory.join("preferences.json"))
                .and_then(|m| m.modified())
                .ok();
            if modified != self.preferences_modified {
                if let Ok(bytes) = std::fs::read(self.directory.join("preferences.json"))
                    && let Ok(preferences) = serde_json::from_slice::<Preferences>(&bytes)
                {
                    self.preferences.theme = preferences.theme;
                    self.preferences.design = preferences.design;
                    self.apply_appearance(cx);
                    changed = true;
                }
                self.preferences_modified = modified;
            }
        }
        while let Ok(update) = self.rx.try_recv() {
            changed = true;
            match update {
                Update::Snapshot(selected, snapshot) => {
                    self.connected = true;
                    if selected == self.selected {
                        self.snapshot = snapshot;
                    } else {
                        self.snapshot.workspaces = snapshot.workspaces;
                        self.snapshot.sessions = snapshot.sessions;
                    }
                    for session in &self.snapshot.sessions {
                        for editor in session
                            .approvals
                            .iter()
                            .filter(|approval| approval.requirement["kind"] == "plank_editor")
                        {
                            self.documents
                                .entry(document_key(&session.id, editor))
                                .or_insert_with(|| {
                                    crate::document::Document::new(&editor.arguments)
                                });
                        }
                    }
                    #[cfg(target_family = "wasm")]
                    self.browser_exports.retain(|key, (url, _)| {
                        let current = snapshot_has_export(&self.snapshot, key);
                        if !current {
                            let _ = web_sys::Url::revoke_object_url(url);
                        }
                        current
                    });
                    if self.selected.is_none()
                        && let Some(session) = self.snapshot.sessions.iter().find(|s| !s.archived)
                    {
                        self.select(session.id.clone());
                    }
                }
                Update::Selected(id) => {
                    self.select(id);
                    self.show_new = false;
                    self.error = None;
                }
                Update::Error(error) => {
                    if error.starts_with("Daemon disconnected:") {
                        self.connected = false;
                    }
                    self.error = Some(error);
                }
                Update::WorkspaceCreated(id) => {
                    self.workspace = Some(id);
                    self.error = None;
                }
                Update::Accepted(prompt) => {
                    self.error = None;
                    if let Some((id, text)) = prompt
                        && self.drafts.get(&id) == Some(&text)
                    {
                        self.drafts.remove(&id);
                        self.pending_prompts.remove(&id);
                        self.save_preferences();
                    }
                }
                Update::ExportFinished(key, result) => {
                    self.export_jobs.remove(&key);
                    self.export_feedback = Some((
                        key,
                        result.unwrap_or_else(|error| format!("Download failed: {error}")),
                    ));
                }
                #[cfg(target_family = "wasm")]
                Update::ExportReady(key, url, name) => {
                    self.export_jobs.remove(&key);
                    let current = snapshot_has_export(&self.snapshot, &key);
                    if current {
                        // Keep only one prepared file; large Blobs must not accumulate.
                        for (_, (old, _)) in self.browser_exports.drain() {
                            let _ = web_sys::Url::revoke_object_url(&old);
                        }
                        self.browser_exports.insert(key, (url, name));
                    } else {
                        let _ = web_sys::Url::revoke_object_url(&url);
                    }
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn select(&mut self, id: String) {
        self.command(Command::AcknowledgeSession {
            session_id: id.clone(),
        });
        self.workspace = self
            .snapshot
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.workspace_id.clone());
        self.selected = Some(id.clone());
        self.show_tasks = false;
        self.activity_tab = ActivityTab::Tasks;
        self.extension_kind = "plugins";
        self.extension_template = None;
        self.model_view = "settings";
        self.saved_current = false;
        self.metrics_kind = "context";
        self.history_checkpoints = true;
        self.history_tree = false;
        self.task_full_report = false;
        self.task_edit_id = None;
        self.show_commands = false;
        self.command_dismissed = false;
        self.command_cursor = 0;
        self.preferences.selected = Some(id.clone());
        self.before = 0;
        self.snapshot.entries.clear();
        self.mobile_sidebar = false;
        if let Some(session) = self.snapshot.sessions.iter().find(|s| s.id == id) {
            self.fields
                .insert("rename-session".into(), session.name.clone());
            if let Some(workspace) = self
                .snapshot
                .workspaces
                .iter()
                .find(|w| w.id == session.workspace_id)
            {
                self.fields
                    .insert("rename-workspace".into(), workspace.name.clone());
            }
        }
        let _ = self.tx.send(Operation::Select(Some(id), 0));
        self.save_preferences();
    }

    fn command(&mut self, command: Command) {
        if self.tx.send(Operation::Command(command)).is_err() {
            self.error = Some("Connection worker stopped; reopen the window".into());
        }
    }

    fn download_export(
        &mut self,
        session: String,
        generation: u64,
        export: crate::model::EngineExport,
        cx: &mut Context<Self>,
    ) {
        let key = export_key(&session, generation, &export.id);
        #[cfg(target_family = "wasm")]
        {
            if !self.export_jobs.is_empty() {
                self.export_feedback = Some((
                    key,
                    "A file is already downloading; retry when it finishes.".into(),
                ));
                cx.notify();
                return;
            }
            for (_, (url, _)) in self.browser_exports.drain() {
                let _ = web_sys::Url::revoke_object_url(&url);
            }
        }
        if !export.is_valid() || !self.export_jobs.insert(key.clone()) {
            return;
        }
        self.export_feedback = None;
        #[cfg(not(target_family = "wasm"))]
        {
            let directory = std::env::current_dir().unwrap_or_default();
            let picker = cx.prompt_for_new_path(&directory, Some(&export.suggested_name()));
            cx.spawn(async move |entity, cx| {
                let choice = picker.await;
                let _ = entity.update(cx, |view, cx| {
                    match choice {
                        Ok(Ok(Some(path))) => {
                            if view
                                .tx
                                .send(Operation::Download(ExportDownload {
                                    key: key.clone(),
                                    session,
                                    generation,
                                    export,
                                    path,
                                }))
                                .is_err()
                            {
                                view.export_jobs.remove(&key);
                                view.export_feedback =
                                    Some((key, "Download worker stopped".into()));
                            }
                        }
                        Ok(Ok(None)) => {
                            view.export_jobs.remove(&key);
                        }
                        error => {
                            view.export_jobs.remove(&key);
                            view.export_feedback =
                                Some((key, format!("Could not choose a destination: {error:?}")));
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        #[cfg(target_family = "wasm")]
        if self
            .tx
            .send(Operation::Download(ExportDownload {
                key: key.clone(),
                session,
                generation,
                export,
            }))
            .is_err()
        {
            self.export_jobs.remove(&key);
            self.export_feedback = Some((key, "Download worker stopped".into()));
        }
        cx.notify();
    }

    fn submit(&mut self) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let text = self.drafts.get(&id).cloned().unwrap_or_default();
        if text.trim().is_empty() {
            return;
        }
        let request_id = match self.pending_prompts.get(&id) {
            Some((request_id, previous)) if previous == &text => request_id.clone(),
            _ => {
                let request_id = uuid::Uuid::new_v4().to_string();
                self.pending_prompts
                    .insert(id.clone(), (request_id.clone(), text.clone()));
                request_id
            }
        };
        if self.snapshot.sessions.iter().any(|session| {
            session.id == id
                && matches!(
                    session.backend,
                    Backend::Local { .. } | Backend::Remote { .. }
                )
        }) {
            let mut words = text.split_whitespace();
            let tab = match words.next() {
                Some("/config") if words.next() != Some("--resolved") => Some(ActivityTab::Config),
                Some("/list") => Some(ActivityTab::Sessions),
                Some("/export") => {
                    self.saved_current = true;
                    Some(ActivityTab::Sessions)
                }
                Some("/jobs") => Some(ActivityTab::Jobs),
                Some("/model") => {
                    self.model_view = "downloads";
                    Some(ActivityTab::Model)
                }
                Some("/engines") => {
                    self.model_view = "engines";
                    Some(ActivityTab::Model)
                }
                Some(command @ ("/context" | "/usage" | "/toks" | "/stats" | "/insights")) => {
                    self.model_view = "metrics";
                    self.metrics_kind = match command {
                        "/usage" => "usage",
                        "/toks" => "throughput",
                        "/stats" => "activity",
                        "/insights" => "insights",
                        _ => "context",
                    };
                    Some(ActivityTab::Model)
                }
                Some("/rc" | "/remote-control" | "/grant" | "/remote") => {
                    self.model_view = "remote";
                    Some(ActivityTab::Model)
                }
                Some("/kvcache") => Some(ActivityTab::Cache),
                Some("/tasks" | "/goal") => Some(ActivityTab::Tasks),
                Some(
                    command
                    @ ("/plugins" | "/skills" | "/templates" | "/hooks" | "/mcp" | "/agent"),
                ) => {
                    self.extension_kind = match command {
                        "/skills" => "skills",
                        "/templates" => "templates",
                        "/hooks" => "hooks",
                        "/mcp" => "mcp",
                        "/agent" => "agents",
                        _ => "plugins",
                    };
                    Some(ActivityTab::Extensions)
                }
                Some("/checkpoint" | "/rollback") => {
                    self.history_checkpoints = true;
                    Some(ActivityTab::History)
                }
                Some("/tree" | "/fork" | "/clone") => {
                    self.history_checkpoints = false;
                    Some(ActivityTab::History)
                }
                _ => None,
            };
            if let Some(tab) = tab {
                self.activity_tab = tab;
                self.show_tasks = true;
            }
        }
        self.save_preferences();
        self.command(Command::Prompt {
            session_id: id.clone(),
            request_id,
            text,
        });
        // Keep the draft until the user changes it, including after a rejected command.
    }

    fn field(&self, name: &str) -> String {
        self.fields.get(name).cloned().unwrap_or_default()
    }

    fn controls(&self) -> &[crate::commands::PlankCommand] {
        self.snapshot
            .sessions
            .iter()
            .find(|session| Some(&session.id) == self.selected.as_ref())
            .and_then(|session| session.controls.as_ref())
            .map(|controls| controls.commands.as_slice())
            .unwrap_or(crate::commands::catalog())
    }

    fn command_popup_visible(&self) -> bool {
        if !self.snapshot.sessions.iter().any(|session| {
            Some(&session.id) == self.selected.as_ref()
                && matches!(
                    session.backend,
                    Backend::Local { .. } | Backend::Remote { .. }
                )
        }) {
            return false;
        }
        let draft = self
            .selected
            .as_ref()
            .and_then(|id| self.drafts.get(id))
            .map(String::as_str)
            .unwrap_or_default();
        !self.command_dismissed
            && (self.show_commands
                || (draft.starts_with('/')
                    && !draft.contains(char::is_whitespace)
                    && !self.controls().iter().any(|command| command.name == draft)))
    }

    fn control_key(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        #[cfg(not(target_os = "macos"))]
        if key == "p"
            && event.keystroke.modifiers.shift
            && (event.keystroke.modifiers.control || event.keystroke.modifiers.platform)
        {
            self.open_commands(cx);
            cx.stop_propagation();
            return;
        }
        if key == "escape" && self.show_new {
            self.show_new = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !self.command_popup_visible()
            || !(self.composer_focus.is_focused(window) || self.command_focus.is_focused(window))
        {
            return;
        }
        let query = if self.show_commands {
            self.command_query.as_str()
        } else {
            self.selected
                .as_ref()
                .and_then(|id| self.drafts.get(id))
                .map(String::as_str)
                .unwrap_or_default()
        };
        let matches = crate::commands::matching_in(self.controls(), query);
        let count = matches.len();
        let selected = matches
            .get(self.command_cursor.min(count.saturating_sub(1)))
            .map(|command| (command.name.clone(), !command.args.is_empty()));
        match key {
            "up" => self.command_cursor = self.command_cursor.saturating_sub(1),
            "down" => self.command_cursor = (self.command_cursor + 1).min(count.saturating_sub(1)),
            "tab" | "enter" if selected.is_some() => {
                let (name, has_args) = selected.unwrap();
                self.prepare_command(&name, has_args, cx);
                self.composer_focus.focus(window, cx);
            }
            "escape" => {
                self.show_commands = false;
                self.command_dismissed = true;
                self.composer_focus.focus(window, cx);
            }
            _ => return,
        }
        self.command_scroll.scroll_to_item(self.command_cursor);
        cx.stop_propagation();
        cx.notify();
    }

    fn reload_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self
            .snapshot
            .sessions
            .iter()
            .find(|session| Some(&session.id) == self.selected.as_ref())
            && !session.state.busy()
            && !session.recovery_required
            && !session.archived
        {
            self.command(Command::Prompt {
                session_id: session.id.clone(),
                request_id: uuid::Uuid::new_v4().to_string(),
                text: "/commands".into(),
            });
            cx.notify();
        }
    }

    fn command_picker(&self, draft: &str, cx: &Context<Self>) -> gpui::AnyElement {
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let query = if self.show_commands {
            self.command_query.as_str()
        } else {
            draft
        };
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .bg(colors.surface)
            .border_1()
            .border_color(colors.muted)
            .rounded(px(5.0));
        let live = self
            .snapshot
            .sessions
            .iter()
            .find(|session| Some(&session.id) == self.selected.as_ref())
            .is_some_and(|session| session.controls.is_some());
        panel = panel.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .text_size(px(10.0))
                .text_color(colors.muted)
                .child(if live {
                    "Engine controls"
                } else {
                    "Bundled controls · load engine for extensions"
                })
                .child(
                    Button::new("reload-controls", "Load")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(self.snapshot.sessions.iter().any(|session| {
                            Some(&session.id) == self.selected.as_ref()
                                && (session.state.busy()
                                    || session.recovery_required
                                    || session.archived)
                        }))
                        .on_click(click(cx, |view, _, cx| view.reload_controls(cx))),
                ),
        );
        if self.show_commands {
            let weak = cx.entity().downgrade();
            panel = panel.child(
                Input::new("command-search")
                    .focus_handle(self.command_focus.clone())
                    .placeholder("Find a Plank control…")
                    .value(self.command_query.clone())
                    .on_text_change(move |text, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            view.command_query = text.into();
                            view.command_cursor = 0;
                            view.command_scroll.scroll_to_item(0);
                            cx.notify();
                        });
                    }),
            );
        }
        let mut rows = div()
            .id("command-results")
            .max_h(px(180.0))
            .overflow_y_scroll()
            .track_scroll(&self.command_scroll)
            .flex()
            .flex_col()
            .gap_1();
        let matches = crate::commands::matching_in(self.controls(), query);
        if matches.is_empty() {
            rows = rows.child(
                div()
                    .text_xs()
                    .text_color(colors.muted)
                    .child("Custom skills and template commands can still be submitted directly."),
            );
        }
        for (index, command) in matches.into_iter().enumerate() {
            let name = command.name.clone();
            let has_args = !command.args.is_empty();
            let mut description = div().flex_1().min_w_0().text_color(colors.muted).child(
                div().text_ellipsis().child(format!(
                    "{}{}",
                    command.description,
                    if command.source == "builtin" {
                        String::new()
                    } else {
                        format!(" · {}", command.source)
                    }
                )),
            );
            if let Some(requirement) = &command.requirement {
                description = description.child(div().text_ellipsis().child(requirement.clone()));
            }
            rows =
                rows.child(
                    div()
                        .id(SharedString::from(format!("control-{}", command.name)))
                        .flex()
                        .gap_2()
                        .px_2()
                        .py_1()
                        .cursor_pointer()
                        .text_size(px(11.0))
                        .border_l_2()
                        .border_color(if index == self.command_cursor {
                            colors.accent
                        } else {
                            colors.surface
                        })
                        .hover(|style| style.bg(colors.background))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.prepare_command(&name, has_args, cx)
                        }))
                        .child(
                            div()
                                .w(px(185.0))
                                .flex_shrink_0()
                                .child(format!("{} {}", command.name, command.args)),
                        )
                        .child(description),
                );
        }
        panel.child(rows).into_any_element()
    }

    fn input(&self, name: &'static str, label: &'static str, cx: &Context<Self>) -> Input {
        let weak = cx.entity().downgrade();
        Input::new(name)
            .label(label)
            .value(self.field(name))
            .on_text_change(move |text, _, cx| {
                let _ = weak.update(cx, |view, _| {
                    view.fields.insert(name.into(), text.into());
                });
            })
    }

    fn create_session(&mut self, backend: usize) {
        let workspace_id = self
            .workspace
            .clone()
            .or_else(|| {
                self.selected
                    .as_ref()
                    .and_then(|id| self.snapshot.sessions.iter().find(|s| &s.id == id))
                    .map(|s| s.workspace_id.clone())
            })
            .or_else(|| self.snapshot.workspaces.first().map(|w| w.id.clone()));
        let Some(workspace_id) = workspace_id else {
            self.error = Some("Create a workspace first".into());
            return;
        };
        let backend = match backend {
            0 => Backend::Demo,
            1 => {
                let args = match serde_json::from_str::<Vec<String>>(&self.field("arguments")) {
                    Ok(args) => args,
                    Err(error) => {
                        self.error =
                            Some(format!("Arguments must be a JSON string array: {error}"));
                        return;
                    }
                };
                Backend::Local {
                    executable: self.field("executable"),
                    args,
                }
            }
            2 => Backend::Remote {
                url: self.field("remote-url"),
                token_env: self.field("remote-token-env"),
            },
            choice => {
                let provider = provider_choice(choice);
                let args = match serde_json::from_str::<Vec<String>>(&self.field("agent-arguments"))
                {
                    Ok(args) => args,
                    Err(error) => {
                        self.error =
                            Some(format!("Arguments must be a JSON string array: {error}"));
                        return;
                    }
                };
                let executable = self.field("agent-executable");
                let model = self.field("agent-model");
                Backend::Agent {
                    provider,
                    executable: if executable.trim().is_empty() {
                        self.snapshot
                            .providers
                            .iter()
                            .find(|p| p.provider == provider)
                            .and_then(|p| p.executable.clone())
                            .unwrap_or_else(|| provider.executable().into())
                    } else {
                        executable
                    },
                    args,
                    model: (!model.trim().is_empty()).then_some(model),
                }
            }
        };
        self.command(Command::CreateSession {
            workspace_id,
            name: self.field("session-name"),
            backend,
        });
    }

    fn save_preferences(&mut self) {
        self.preferences.documents = self.documents.clone();
        self.preferences.drafts = self.drafts.clone();
        self.preferences.collapsed = self.collapsed.clone();
        self.preferences.pending_prompts = self.pending_prompts.clone();
        #[cfg(not(target_family = "wasm"))]
        let result = (|| -> anyhow::Result<()> {
            use std::io::Write;
            let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
            file.write_all(&serde_json::to_vec(&self.preferences)?)?;
            file.as_file().sync_all()?;
            file.persist(self.directory.join("preferences.json"))?;
            Ok(())
        })();
        #[cfg(target_family = "wasm")]
        let result = (|| -> anyhow::Result<()> {
            let storage = web_sys::window()
                .and_then(|window| window.local_storage().ok().flatten())
                .ok_or_else(|| anyhow::anyhow!("Browser preference storage is unavailable"))?;
            storage
                .set_item(
                    "nplank.preferences",
                    &serde_json::to_string(&self.preferences)?,
                )
                .map_err(|_| anyhow::anyhow!("Browser preferences could not be saved"))?;
            Ok(())
        })();
        if let Err(error) = result {
            self.error = Some(format!("Could not save preferences: {error}"));
        }
    }

    fn apply_appearance(&self, cx: &mut App) {
        let variants = ThemeVariant::all();
        cx.set_global(ThemeState::with_variant(
            variants[self.preferences.theme % variants.len()],
        ));
        cx.set_global(DesignSystemState::with_system(design(
            self.preferences.design,
        )));
    }

    fn appearance_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = ThemeVariant::all()[self.preferences.theme % ThemeVariant::all().len()].name();
        layout! { V[g6] {
            Hd "Preferences" [level="2"],
            Tx.sm "Colors and design rules are independently selectable." [muted],
        }}
        .child(div().child(format!("Theme: {theme}")))
        .child(layout! { B.primary("Next theme") #next_theme }.on_click(click(cx, |view, _, cx| {
            view.preferences.theme += 1; view.apply_appearance(cx); view.save_preferences(); cx.notify();
        })))
        .child(div().child(format!("Design: {}", design_name(self.preferences.design))))
        .child(layout! { B("Next design") #next_design }.on_click(click(cx, |view, _, cx| {
            view.preferences.design = (view.preferences.design + 1) % 7; view.apply_appearance(cx); view.save_preferences(); cx.notify();
        })))
        .child(layout! { V[g4] { Hd "Session preview" [level="3"], Tx "Research · Local Plank", Tx.sm "Running · 32 tokens/s" [muted] }})
        .child(Markdown::new("appearance-preview", "### An answer from Plank\n\nTheme changes apply to session views and controls.\n\n```rust\nfn main() { println!(\"Hello, Plank\"); }\n```"))
        .into_any_element()
    }

    fn new_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut panel = layout! { V[g4] { Hd "Add agent" [level="2"] }};
        if self.snapshot.workspaces.is_empty() {
            panel =
                panel.child(layout! { Tx "Create your first workspace to get started." [muted] });
        }
        if self.snapshot.workspaces.is_empty() || self.advanced {
            panel = panel
                .child(self.input("workspace-name", "Workspace name", cx))
                .child(self.input("directory", "Working directory on the daemon host", cx))
                .child(
                    layout! { B("Create workspace") #create_workspace }.on_click(click(
                        cx,
                        |view, _, cx| {
                            view.command(Command::CreateWorkspace {
                                name: view.field("workspace-name"),
                                directory: view.field("directory"),
                            });
                            cx.notify();
                        },
                    )),
                );
        }
        panel = panel.child(self.input("session-name", "Agent name", cx));
        let mut workspaces = div().flex().flex_wrap().gap_2();
        for workspace in &self.snapshot.workspaces {
            let id = workspace.id.clone();
            workspaces = workspaces.child(
                Button::new(
                    SharedString::from(format!("target-{id}")),
                    workspace.name.clone(),
                )
                .variant(if self.workspace.as_ref() == Some(&id) {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                })
                .on_click(click(cx, move |view, _, cx| {
                    view.workspace = Some(id.clone());
                    cx.notify();
                })),
            );
        }
        panel = panel.child(workspaces);
        let mut picker = div().flex().flex_wrap().gap_2();
        for (choice, label) in [
            (1, "Plank"),
            (3, "Muse Code"),
            (4, "Codex"),
            (5, "Claude Code"),
            (2, "Remote Plank"),
            (0, "Demo"),
        ] {
            picker = picker.child(
                Button::new(SharedString::from(format!("provider-{choice}")), label)
                    .variant(if self.agent_choice == choice {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Secondary
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.agent_choice = choice;
                        view.fields.insert("agent-executable".into(), String::new());
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(picker);
        match self.agent_choice {
            1 => {
                panel = panel
                    .child(self.input("executable", "Plank executable", cx))
                    .child(self.input(
                        "arguments",
                        "Plank engine arguments (JSON string array)",
                        cx,
                    ));
            }
            2 => {
                panel = panel
                    .child(self.input("remote-url", "Plank /rc URL through SSH", cx))
                    .child(self.input(
                        "remote-token-env",
                        "Token environment variable in the daemon",
                        cx,
                    ));
            }
            3..=5 => {
                let provider = provider_choice(self.agent_choice);
                let detected = self
                    .snapshot
                    .providers
                    .iter()
                    .find(|p| p.provider == provider)
                    .and_then(|p| p.executable.as_deref());
                panel = panel.child(div().text_sm().child(match detected {
                    Some(path) => format!("Detected on daemon: {path}"),
                    None => format!("{} is not installed on the daemon host. Set an executable under Advanced.", provider.label()),
                })).child(self.input("agent-model", "Model (leave empty for the agent default)", cx));
            }
            _ => {}
        }
        panel = panel.child(
            layout! { B.ghost("Advanced launch settings") #advanced }.on_click(click(
                cx,
                |view, _, cx| {
                    view.advanced = !view.advanced;
                    cx.notify();
                },
            )),
        );
        if self.advanced && self.agent_choice >= 3 {
            panel = panel.child(self.input("agent-executable", "Executable override (absolute path)", cx))
                .child(self.input("agent-arguments", "Extra CLI arguments (JSON string array)", cx))
                .child(layout! { Tx.sm "The agent’s saved login and permission settings apply. Protocol and resume flags are managed by nplank." [muted] });
        }
        panel
            .child(
                layout! { B.primary("Create agent") #create_agent }
                    .disabled(self.snapshot.workspaces.is_empty())
                    .on_click(click(cx, |view, _, cx| {
                        view.create_session(view.agent_choice);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn sidebar(&self, colors: &WorkspaceTheme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mut sidebar = div()
            .id("workspace-sidebar")
            .flex()
            .flex_col()
            .gap_3()
            .p_2()
            .h_full()
            .overflow_y_scroll()
            .bg(colors.surface);
        #[cfg(not(target_os = "macos"))]
        {
            sidebar = sidebar.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("new-agent", "+")
                            .on_click(click(cx, |view, _, cx| view.add_agent(None, cx))),
                    )
                    .child(
                        Button::new("preferences", "⚙")
                            .variant(ButtonVariant::Ghost)
                            .on_click(click(cx, |view, _, cx| view.open_preferences(cx))),
                    ),
            );
        }
        for workspace in &self.snapshot.workspaces {
            let hue = workspace
                .id
                .bytes()
                .fold(0u32, |hash, b| hash.wrapping_mul(31).wrapping_add(b as u32))
                % 360;
            let accent = gpui::hsla(hue as f32 / 360.0, 0.65, 0.55, 1.0);
            let tint = gpui::hsla(hue as f32 / 360.0, 0.65, 0.55, 0.14);
            let collapsed = self.collapsed.contains(&workspace.id);
            let select_id = workspace.id.clone();
            let add_id = workspace.id.clone();
            let fold_id = workspace.id.clone();
            let mut group = div()
                .rounded(px(7.0))
                .border_1()
                .border_l_4()
                .border_color(accent)
                .bg(tint)
                .overflow_hidden();
            let header = div()
                .flex()
                .items_center()
                .px_2()
                .py_1()
                .gap_1()
                .child(
                    div()
                        .id(SharedString::from(format!("workspace-{}", workspace.id)))
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_ellipsis()
                        .child(workspace.name.clone())
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.workspace = Some(select_id.clone());
                            cx.notify();
                        })),
                )
                .child(
                    Button::new(SharedString::from(format!("add-{}", workspace.id)), "+")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .on_click(click(cx, move |view, _, cx| {
                            view.add_agent(Some(add_id.clone()), cx)
                        })),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("fold-{}", workspace.id)),
                        if collapsed { "▸" } else { "▾" },
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, move |view, _, cx| {
                        if !view.collapsed.remove(&fold_id) {
                            view.collapsed.insert(fold_id.clone());
                        }
                        view.save_preferences();
                        cx.notify();
                    })),
                );
            group = group.child(header);
            if !collapsed {
                let mut sessions = div().flex().flex_col().gap_1().p_1();
                for session in self.snapshot.sessions.iter().filter(|session| {
                    session.workspace_id == workspace.id
                        && (!session.archived || self.show_archived)
                }) {
                    let id = session.id.clone();
                    let selected = self.selected.as_ref() == Some(&id);
                    let attention = session.attention
                        || session.recovery_required
                        || !session.approvals.is_empty();
                    let status = if !session.approvals.is_empty() {
                        "Needs approval"
                    } else if session.recovery_required {
                        "Needs recovery"
                    } else if session.state.busy() {
                        "Running"
                    } else if session.completed_at.is_some() {
                        "Done"
                    } else {
                        "Ready"
                    };
                    let mut card = div()
                        .id(SharedString::from(format!("session-{id}")))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .p_2()
                        .rounded(px(5.0))
                        .cursor_pointer()
                        .min_w_0()
                        .border_1()
                        .border_color(if attention {
                            accent
                        } else if selected {
                            colors.muted.into()
                        } else {
                            gpui::hsla(0.0, 0.0, 0.0, 0.0)
                        })
                        .bg(if selected {
                            colors.background
                        } else {
                            colors.surface
                        })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.select(id.clone());
                            view.show_new = false;
                            view.appearance = false;
                            cx.notify();
                        }));
                    if attention {
                        card = card.shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(hue as f32 / 360.0, 0.75, 0.6, 0.45),
                            offset: gpui::point(px(0.0), px(0.0)),
                            blur_radius: px(9.0),
                            spread_radius: px(1.0),
                            inset: false,
                        }]);
                    }
                    card = card
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .items_center()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_size(px(12.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(session.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(if attention {
                                            accent.into()
                                        } else {
                                            colors.muted
                                        })
                                        .child(status),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(colors.muted)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(if session.summary.is_empty() {
                                    backend_label(&session.backend).to_owned()
                                } else {
                                    session.summary.clone()
                                }),
                        );
                    if let Some(context) = self.snapshot.contexts.get(&session.id) {
                        if let Some(branch) = &context.branch {
                            card = card.child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(colors.muted)
                                    .child(format!(
                                        "⑂ {branch}{}",
                                        if context.dirty { "*" } else { "" }
                                    )),
                            );
                        }
                        if let Some(ports) = &context.ports
                            && !ports.is_empty()
                        {
                            card = card.child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(colors.muted)
                                    .child(format!(
                                        "Ports {}",
                                        ports
                                            .iter()
                                            .map(u16::to_string)
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    )),
                            );
                        }
                    }
                    card = card.child(
                        div()
                            .text_size(px(10.0))
                            .text_color(colors.muted)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(workspace.directory.clone()),
                    );
                    sessions = sessions.child(card);
                }
                group = group.child(sessions);
            }
            sidebar = sidebar.child(group);
        }
        if self.snapshot.workspaces.is_empty() {
            sidebar = sidebar.child(
                Button::new("first-workspace", "+ Workspace")
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, |view, _, cx| view.add_workspace(cx))),
            );
        }
        sidebar.into_any_element()
    }

    fn conversation(
        &self,
        session: &Session,
        colors: &WorkspaceTheme,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut entries: Vec<(i64, String, String)> = Vec::new();
        for Entry {
            sequence,
            role,
            text,
            ..
        } in &self.snapshot.entries
        {
            if let Some(last) = entries.last_mut().filter(|last| last.1 == *role) {
                last.2.push_str(text);
            } else {
                entries.push((*sequence, role.clone(), text.clone()));
            }
        }
        let mut transcript = div()
            .id("transcript")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .p_2();
        if entries.is_empty() {
            transcript = transcript.child(layout! { V[g4] { Hd "A place to think and build" [level="2"], Tx "Send a prompt to begin. Runs belong to the daemon and survive window closure." [muted] }});
        }
        if self.snapshot.entries.len() >= 250 {
            transcript = transcript.child(layout! { B.ghost("Older messages") #older }.on_click(
                click(cx, |view, _, cx| {
                    if let Some(first) = view.snapshot.entries.first() {
                        view.before = first.sequence;
                        let _ = view
                            .tx
                            .send(Operation::Select(view.selected.clone(), view.before));
                    }
                    cx.notify();
                }),
            ));
        }
        if self.before != 0 {
            transcript = transcript.child(layout! { B("Return to live output") #live }.on_click(
                click(cx, |view, _, cx| {
                    view.before = 0;
                    let _ = view.tx.send(Operation::Select(view.selected.clone(), 0));
                    cx.notify();
                }),
            ));
        }
        for (sequence, role, text) in entries {
            let folded = matches!(
                role.as_str(),
                "tool"
                    | "thinking"
                    | "diagnostic"
                    | "aside_thinking"
                    | "aside_tool"
                    | "aside_notice"
            );
            if folded {
                let expanded = self.expanded.contains(&sequence);
                transcript = transcript.child(
                    Button::new(
                        SharedString::from(format!("fold-{sequence}")),
                        format!(
                            "{} {}",
                            if expanded { "▾" } else { "▸" },
                            role.to_uppercase()
                        ),
                    )
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, move |view, _, cx| {
                        if !view.expanded.remove(&sequence) {
                            view.expanded.insert(sequence);
                        }
                        cx.notify();
                    })),
                );
                if !expanded {
                    continue;
                }
            }
            transcript = transcript.child(
                div()
                    .p_2()
                    .rounded(px(cx.design().corners.md))
                    .bg(colors.surface)
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors.muted)
                            .child(role.replace('_', " ").to_uppercase()),
                    )
                    .child(Markdown::new(
                        SharedString::from(format!("entry-{sequence}")),
                        text,
                    )),
            );
        }
        let id = session.id.clone();
        let weak = cx.entity().downgrade();
        let draft_id = id.clone();
        let prompt = layout! { I #prompt [placeholder="Message your agent…"] }
            .focus_handle(self.composer_focus.clone())
            .value(self.drafts.get(&id).cloned().unwrap_or_default())
            .disabled(session.archived)
            .on_text_change(move |text, _, cx| {
                let _ = weak.update(cx, |view, cx| {
                    view.drafts.insert(draft_id.clone(), text.into());
                    view.draft_dirty = true;
                    view.command_cursor = 0;
                    view.command_dismissed = false;
                    cx.notify();
                });
            })
            .on_change({
                let weak = cx.entity().downgrade();
                move |text, _, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        if let Some(id) = &view.selected {
                            view.drafts.insert(id.clone(), text.into());
                        }
                        if !view.snapshot.sessions.iter().any(|s| {
                            Some(&s.id) == view.selected.as_ref()
                                && (s.state.busy() || s.recovery_required || s.archived)
                        }) {
                            view.submit();
                        }
                        cx.notify();
                    });
                }
            });
        let stop_id = id.clone();
        let reset_id = id.clone();
        let mut composer = layout! { V[g2] {} }.child(prompt).child(
            layout! { H[g2] {} }
                .child(
                    Button::new("plank-commands", "/")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(!matches!(
                            session.backend,
                            Backend::Local { .. } | Backend::Remote { .. }
                        ))
                        .on_click(click(cx, |view, _, cx| view.open_commands(cx))),
                )
                .child(
                    layout! { B.primary("Send") #send }
                        .size(ButtonSize::Xs)
                        .disabled(
                            session.state.busy() || session.recovery_required || session.archived,
                        )
                        .on_click(click(cx, |view, _, cx| {
                            view.submit();
                            cx.notify();
                        })),
                )
                .child(
                    layout! { B.destructive("Stop") #stop }
                        .size(ButtonSize::Xs)
                        .disabled(!session.state.busy())
                        .on_click(click(cx, move |view, _, cx| {
                            view.command(Command::Stop {
                                session_id: stop_id.clone(),
                            });
                            cx.notify();
                        })),
                )
                .child(
                    layout! { B.ghost("Fresh context") #reset }
                        .size(ButtonSize::Xs)
                        .disabled(session.state.busy())
                        .on_click(click(cx, move |view, _, cx| {
                            view.command(Command::Reset {
                                session_id: reset_id.clone(),
                            });
                            cx.notify();
                        })),
                ),
        );
        if session.progress.is_some() {
            composer = composer.child(
                div()
                    .text_xs()
                    .text_color(colors.muted)
                    .child(session.summary.clone()),
            );
        }
        if matches!(
            session.backend,
            Backend::Local { .. } | Backend::Remote { .. }
        ) {
            let draft = self.drafts.get(&id).map(String::as_str).unwrap_or_default();
            if self.command_popup_visible() {
                composer = composer.child(self.command_picker(draft, cx));
            }
        }
        if session.recovery_required {
            composer = composer.child(layout! { Tx.sm "Live engine context was lost. Fresh context preserves the transcript but starts a new conversation." [muted] });
            if session.resumable && session.agent_session.is_some() {
                let id = session.id.clone();
                composer = composer.child(layout! { B("Resume saved context") #resume }.on_click(
                    click(cx, move |view, _, cx| {
                        view.command(Command::Resume {
                            session_id: id.clone(),
                        });
                        cx.notify();
                    }),
                ));
            }
        }
        for approval in &session.approvals {
            if approval.requirement["kind"] == "plank_editor" {
                composer = composer.child(self.document_editor(&session.id, approval, cx));
                continue;
            }
            let plank = approval.requirement["kind"] == "plank_question";
            let multi = plank && approval.requirement["request"]["multi"] == true;
            let selection_key = format!(
                "question-{}-{}-{}",
                session.id, session.generation, approval.id
            );
            let selected = self.field(&selection_key);
            let mut controls = div().flex().flex_wrap().gap_2();
            for choice in &approval.choices {
                let id = session.id.clone();
                let approval_id = approval.id.clone();
                let choice_id = choice.id.clone();
                let requirement = approval.requirement.clone();
                let key = selection_key.clone();
                let toggle = multi && choice_id != "0";
                let label = if toggle {
                    format!(
                        "{} {}",
                        if selected.split(',').any(|id| id == choice_id) {
                            "☑"
                        } else {
                            "☐"
                        },
                        choice.label
                    )
                } else {
                    choice.label.clone()
                };
                controls = controls.child(
                    Button::new(
                        SharedString::from(format!("approval-{approval_id}-{choice_id}")),
                        label,
                    )
                    .size(ButtonSize::Xs)
                    .disabled(self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, _, cx| {
                        if toggle {
                            let current = view.field(&key);
                            let mut choices: Vec<_> = current
                                .split(',')
                                .filter(|id| !id.is_empty())
                                .map(str::to_owned)
                                .collect();
                            if choices.iter().any(|id| id == &choice_id) {
                                choices.retain(|id| id != &choice_id);
                            } else {
                                choices.push(choice_id.clone());
                            }
                            view.fields.insert(key.clone(), choices.join(","));
                            cx.notify();
                            return;
                        }
                        view.command(Command::DecideApproval {
                            session_id: id.clone(),
                            approval_id: approval_id.clone(),
                            choice_id: choice_id.clone(),
                            requirement: requirement.clone(),
                        });
                        cx.notify();
                    })),
                );
            }
            if multi {
                let id = session.id.clone();
                let approval_id = approval.id.clone();
                let requirement = approval.requirement.clone();
                controls = controls.child(
                    Button::new(
                        SharedString::from(format!("answer-{approval_id}")),
                        "Answer",
                    )
                    .size(ButtonSize::Xs)
                    .disabled(selected.is_empty() || self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, _, cx| {
                        view.command(Command::DecideApproval {
                            session_id: id.clone(),
                            approval_id: approval_id.clone(),
                            choice_id: view.field(&selection_key),
                            requirement: requirement.clone(),
                        });
                        cx.notify();
                    })),
                );
            }
            composer = composer.child(
                div()
                    .p_2()
                    .child(format!(
                        "{}: {}",
                        if plank { "Question" } else { "Approval needed" },
                        approval.tool
                    ))
                    .child(Markdown::new(
                        SharedString::from(format!("approval-info-{}", approval.id)),
                        if plank {
                            approval.arguments.clone()
                        } else {
                            format!("```json\n{}\n```", approval.arguments)
                        },
                    ))
                    .child(controls),
            );
        }
        div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .child(
                div().p_2().border_b_1().border_color(colors.surface).child(
                    layout! { H[g2] {} }
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(session.name.clone()),
                        )
                        .child(div().text_color(colors.muted).child(format!(
                            "{} · {}",
                            backend_label(&session.backend),
                            state_label(session)
                        )))
                        .child(self.task_toggle(session, cx))
                        .child(
                            layout! { B.ghost("Details") #details }
                                .size(ButtonSize::Xs)
                                .on_click(click(cx, |view, _, cx| {
                                    view.details = !view.details;
                                    cx.notify();
                                })),
                        ),
                ),
            )
            .child({
                let summary = self
                    .snapshot
                    .telemetry
                    .get(&session.id)
                    .map(crate::model::EngineTelemetry::summary)
                    .unwrap_or_default();
                let mut row = div();
                if !summary.is_empty() {
                    row = row.px_2().text_xs().text_color(colors.muted).child(summary);
                }
                row
            })
            .child(self.task_panel(session, cx))
            .child(transcript)
            .child(div().p_2().child(composer))
            .into_any_element()
    }

    fn task_toggle(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        if !matches!(
            session.backend,
            Backend::Local { .. } | Backend::Remote { .. }
        ) {
            return div().into_any_element();
        }
        layout! { B.ghost("Activity") #tasks }
            .size(ButtonSize::Xs)
            .on_click(click(cx, |view, _, cx| {
                view.show_tasks = !view.show_tasks;
                cx.notify();
            }))
            .into_any_element()
    }

    fn task_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        if !self.show_tasks {
            return div().into_any_element();
        }
        let mut tabs = div().flex().flex_wrap().gap_1();
        for (id, label, tab) in [
            ("activity-tasks", "Tasks & goal", ActivityTab::Tasks),
            ("activity-jobs", "Jobs", ActivityTab::Jobs),
            ("activity-agents", "Agents", ActivityTab::Agents),
            ("activity-model", "Model", ActivityTab::Model),
            ("activity-sessions", "Saved", ActivityTab::Sessions),
            ("activity-config", "Settings", ActivityTab::Config),
            ("activity-history", "Branches", ActivityTab::History),
            ("activity-extensions", "Extensions", ActivityTab::Extensions),
            ("activity-cache", "Cache", ActivityTab::Cache),
        ] {
            tabs = tabs.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(if self.activity_tab == tab {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.activity_tab = tab;
                        cx.notify();
                    })),
            );
        }
        if self.activity_tab == ActivityTab::Cache {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.cache_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Extensions {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.extensions_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::History {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.history_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Config {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.config_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Sessions {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.saved_sessions_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Model {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.model_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Jobs {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.jobs_panel(session, cx))
                .into_any_element();
        }
        if self.activity_tab == ActivityTab::Agents {
            return div()
                .px_2()
                .py_1()
                .child(tabs)
                .child(self.subagents_panel(session, cx))
                .into_any_element();
        }
        let report = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.task_report.as_deref())
            .unwrap_or(
                "Task details are unavailable from this engine. Use /tasks to request a report.",
            );
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let task_editable = session.controls.as_ref().is_some_and(|catalog| {
            catalog
                .commands
                .iter()
                .any(|command| command.name == "/tasks" && command.args.contains("set <id>"))
        }) && !self.remote_read_only(&session.id)
            && !session.state.busy()
            && !session.recovery_required
            && !session.archived;
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, args) in [
            ("task-report", "Refresh", "/tasks", false),
            ("task-add", "Add task…", "/tasks add", true),
            ("goal-start", "Set goal", "/goal", true),
            ("goal-report", "Goal status", "/goal", false),
            ("goal-clear", "Clear goal", "/goal clear", false),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Xs)
                    .disabled(
                        self.remote_read_only(&session.id) || (id == "task-add" && !task_editable),
                    )
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        actions = actions.child(
            Button::new(
                "task-detail-view",
                if self.task_full_report {
                    "Task rows"
                } else {
                    "Full report"
                },
            )
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Xs)
            .on_click(click(cx, |view, _, cx| {
                view.task_full_report = !view.task_full_report;
                cx.notify();
            })),
        );
        let rows = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.task_rows.as_ref());
        let content = if !self.task_full_report
            && let Some(rows) = rows
        {
            let mut list = div().flex().flex_col().gap_1().text_xs();
            if let Some(goal) = self
                .snapshot
                .telemetry
                .get(&session.id)
                .and_then(|telemetry| telemetry.goal_label.as_ref())
            {
                list = list.child(goal.clone());
            }
            let total = self
                .snapshot
                .telemetry
                .get(&session.id)
                .and_then(|telemetry| telemetry.tasks)
                .map_or(0, |(_, total)| total);
            list = list.child(format!(
                "{} of {total} tasks · IDs belong to this conversation",
                rows.len()
            ));
            for task in rows {
                let id = task.id;
                let mut row = div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(format!("#{id} · {}", task.subject)),
                    )
                    .child(
                        div()
                            .text_color(colors.muted)
                            .child(task.status.replace('_', " ")),
                    );
                let states: &[(&str, &str)] = match task.status.as_str() {
                    "completed" => &[("Reopen", "pending")],
                    "in_progress" => &[("Pending", "pending"), ("Done", "completed")],
                    _ => &[("Start", "in_progress"), ("Done", "completed")],
                };
                for &(label, status) in states {
                    let command = format!("/tasks set {id} {status}");
                    row = row.child(
                        Button::new(format!("task-{id}-{status}"), label)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Xs)
                            .disabled(!task_editable)
                            .on_click(click(cx, move |view, window, cx| {
                                view.prepare_command(&command, false, cx);
                                view.composer_focus.focus(window, cx);
                                cx.notify();
                            })),
                    );
                }
                row = row.child(
                    Button::new(format!("task-{id}-edit"), "…")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Xs)
                        .disabled(!task_editable)
                        .on_click(click(cx, move |view, _, cx| {
                            view.task_edit_id = if view.task_edit_id == Some(id) {
                                None
                            } else {
                                Some(id)
                            };
                            cx.notify();
                        })),
                );
                list = list.child(row);
                if let Some(active) = &task.active_form {
                    list = list.child(
                        div()
                            .text_color(colors.muted)
                            .text_ellipsis()
                            .child(active.clone()),
                    );
                }
                if self.task_edit_id == Some(id) {
                    let mut edits = div().flex().flex_wrap().gap_1();
                    for (action, label, args) in [
                        ("rename", "Subject…", true),
                        ("active", "Active label…", true),
                        ("active-clear", "Clear active label", false),
                    ] {
                        let command = if action == "active-clear" {
                            format!("/tasks active {id} -")
                        } else {
                            format!("/tasks {action} {id}")
                        };
                        edits = edits.child(
                            Button::new(format!("task-{id}-{action}"), label)
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Xs)
                                .disabled(!task_editable)
                                .on_click(click(cx, move |view, window, cx| {
                                    view.prepare_command(&command, args, cx);
                                    view.composer_focus.focus(window, cx);
                                    cx.notify();
                                })),
                        );
                    }
                    list = list.child(edits);
                }
            }
            list.into_any_element()
        } else {
            Markdown::new("task-goal-report", report.to_owned()).into_any_element()
        };
        div()
            .px_2()
            .py_1()
            .child(tabs)
            .border_b_1()
            .border_color(colors.surface)
            .child(
                div()
                    .id("task-report-scroll")
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .child(content),
            )
            .child(actions)
            .into_any_element()
    }

    fn subagents_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let mut panel = div()
            .id("subagent-activity")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .text_xs();
        panel = panel.child(
            layout! { B.ghost("Delegate task…") #delegate_task }
                .size(ButtonSize::Xs)
                .disabled(self.remote_read_only(&session.id))
                .on_click(click(cx, |view, window, cx| {
                    view.prepare_command("/subagent", true, cx);
                    view.composer_focus.focus(window, cx);
                })),
        );
        if let Some(crate::model::EngineMetrics::Subagents {
            rows,
            total,
            running,
        }) = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.metrics.get("subagents"))
        {
            panel = panel.child(format!("{running} running · {total} retained runs"));
            let selection_key = format!("subagent-report-{}-{}", session.id, session.generation);
            for row in rows {
                panel = panel.child(
                    div()
                        .border_l_2()
                        .border_color(if row.running {
                            colors.accent
                        } else {
                            colors.muted
                        })
                        .pl_2()
                        .child(format!(
                            "{} · {} · {}s",
                            row.label,
                            if row.running { "running" } else { "finished" },
                            row.elapsed_ms / 1000
                        ))
                        .child(div().text_color(colors.muted).child(row.task.clone()))
                        .child(div().text_color(colors.muted).child(format!(
                            "{} input · {} output tokens",
                            row.prefill, row.generated
                        ))),
                );
                if let Some(live) = &row.live {
                    panel = panel.child(div().text_color(colors.muted).child(if live.prefilling {
                        format!(
                            "Prefilling · {}/{} tokens",
                            live.prefill_done, live.prefill_total
                        )
                    } else {
                        format!("Generating · {} streamed pieces in flight", live.generated)
                    }));
                }
                let mut actions = div().flex().flex_wrap().gap_1();
                let mut detail = div();
                for (kind, text) in [
                    ("report", row.report.as_deref()),
                    ("stream", row.output.as_deref()),
                ] {
                    let Some(text) = text else { continue };
                    let id = format!("{}:{kind}", row.id);
                    let expanded = self.fields.get(&selection_key) == Some(&id);
                    let key = selection_key.clone();
                    actions = actions.child(
                        Button::new(
                            format!("subagent-open-{kind}-{}", row.id),
                            if expanded {
                                "Hide"
                            } else if kind == "report" {
                                "Report"
                            } else {
                                "Stream"
                            },
                        )
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .on_click(click(cx, move |view, _, cx| {
                            if expanded {
                                view.fields.remove(&key);
                            } else {
                                view.fields.insert(key.clone(), id.clone());
                            }
                            cx.notify();
                        })),
                    );
                    if expanded {
                        let copied = text.to_owned();
                        detail = detail.child(
                            Button::new(
                                format!("subagent-copy-{kind}-{}", row.id),
                                if kind == "report" {
                                    "Copy report"
                                } else {
                                    "Copy stream tail"
                                },
                            )
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .on_click(click(cx, move |_, _, cx| {
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    copied.clone(),
                                ));
                            })),
                        );
                        if kind == "report" {
                            detail = detail.child(Markdown::new(
                                format!("subagent-report-body-{}", row.id),
                                text.to_owned(),
                            ));
                        } else {
                            if row.output_truncated {
                                detail =
                                    detail.child("Showing the newest 4 KiB of rendered output.");
                            }
                            detail =
                                detail.child(div().font_family(ui_font()).child(text.to_owned()));
                        }
                    }
                }
                panel = panel.child(actions).child(detail);
            }
            if (rows.len() as u64) < *total {
                panel = panel.child("Showing the first 64 runs, active agents first.");
            }
            panel = panel.child("Elapsed time and spend refresh at lifecycle and completed-pass boundaries. Finished means the run ended; inspect its report for the outcome.");
        } else {
            panel = panel.child(
                "Delegated activity will appear here when a rebuilt engine starts a subagent.",
            );
        }
        panel.into_any_element()
    }

    fn cache_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.cache.as_ref());
        let mut panel = div()
            .id("kv-cache-controls")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs();
        let page = snapshot.map_or(1, |snapshot| snapshot.page);
        let pages = snapshot.map_or(1, |snapshot| snapshot.total.div_ceil(32).max(1));
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, args, disabled) in [
            (
                "cache-refresh",
                "Load / refresh",
                format!("/kvcache --page {page}"),
                false,
                false,
            ),
            (
                "cache-previous",
                "‹",
                format!("/kvcache --page {}", page.saturating_sub(1).max(1)),
                false,
                page <= 1,
            ),
            (
                "cache-next",
                "›",
                format!("/kvcache --page {}", page.saturating_add(1)),
                false,
                page >= pages,
            ),
            (
                "cache-sweep",
                "Clean up",
                "/kvcache gc".to_owned(),
                false,
                false,
            ),
            (
                "cache-strip",
                "Strip saved payload…",
                "/strip".to_owned(),
                true,
                false,
            ),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&command, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(actions);
        let Some(snapshot) = snapshot else {
            return panel.child("Load, then Send to inspect the engine host's cache. Older engines retain /kvcache text reports.").into_any_element();
        };
        panel = panel.child(format!("{} · page {page}/{pages}", snapshot.summary));
        for (index, row) in snapshot.rows.iter().enumerate() {
            let mut item = div()
                .pl(px(row.depth.min(16) as f32 * 8.0))
                .py_1()
                .child(row.label.clone())
                .child(row.status.clone())
                .child(row.detail.clone());
            if let Some(pin) = row.command(if row.pinned { "unpin" } else { "pin" }) {
                let mut controls = div().flex().gap_1();
                for (action, label, command) in [
                    ("pin", if row.pinned { "Unpin" } else { "Pin" }, pin),
                    ("rm", "Remove", row.command("rm").unwrap_or_default()),
                ] {
                    controls = controls.child(
                        Button::new(
                            SharedString::from(format!("cache-row-{index}-{action}")),
                            label,
                        )
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .on_click(click(cx, move |view, window, cx| {
                            view.prepare_command(&command, false, cx);
                            view.composer_focus.focus(window, cx);
                            cx.notify();
                        })),
                    );
                }
                item = item.child(controls);
            }
            panel = panel.child(item);
        }
        panel.into_any_element()
    }

    fn extensions_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .id("extension-inventory")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs();
        let mut categories = div().flex().flex_wrap().gap_1();
        for kind in ["plugins", "skills", "templates", "hooks", "mcp", "agents"] {
            categories = categories.child(
                Button::new(
                    SharedString::from(format!("extension-category-{kind}")),
                    kind,
                )
                .size(ButtonSize::Xs)
                .variant(if self.extension_kind == kind {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Ghost
                })
                .on_click(click(cx, move |view, _, cx| {
                    view.extension_kind = kind;
                    view.extension_template = None;
                    cx.notify();
                })),
            );
        }
        panel = panel.child(categories);
        let inventory = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.extensions.get(self.extension_kind));
        if let Some(inventory) = inventory {
            if let Some(enabled) = inventory.enabled {
                panel = panel.child(if enabled {
                    "Enabled for this session"
                } else {
                    "Disabled for this session"
                });
            }
        } else {
            panel = panel.child(
                "Inventory not loaded. Choose Load, then Send to request it from the engine.",
            );
        }
        let report_command = match self.extension_kind {
            "skills" => "/skills",
            "templates" => "/templates",
            "hooks" => "/hooks",
            "mcp" => "/mcp",
            "agents" => "/agent",
            _ => "/plugins",
        };
        let mut actions = div().flex().flex_wrap().gap_1();
        let mut controls = vec![("Load / refresh", report_command, false)];
        match self.extension_kind {
            "plugins" => {
                controls.extend([
                    ("Install…", "/plugins install", true),
                    ("Remove…", "/plugins remove", true),
                    ("Info…", "/plugins info", true),
                    ("Trust…", "/plugins trust", true),
                    ("Enable…", "/plugins enable", true),
                    ("Disable…", "/plugins disable", true),
                    ("Publishers", "/plugins publisher", false),
                    ("Trust publisher…", "/plugins publisher", true),
                    ("Claude plugin…", "/install-claude-plugin", true),
                    ("Profile…", "/install-profile", true),
                ]);
                panel = panel.child("Install/remove and enable/disable take effect on the next engine start. Trust can load a held component now.");
            }
            "skills" => {
                controls.extend([("On", "/skills on", false), ("Off", "/skills off", false)])
            }
            "hooks" => controls.extend([("On", "/hooks on", false), ("Off", "/hooks off", false)]),
            "mcp" => controls.push(("Project config", "/open .mcp.json", false)),
            "agents" => controls.push(("Run task…", "/subagent", true)),
            _ => {}
        }
        for (index, (label, command, args)) in controls.into_iter().enumerate() {
            actions = actions.child(
                Button::new(
                    SharedString::from(format!("extension-action-{index}")),
                    label,
                )
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .disabled(self.remote_read_only(&session.id))
                .on_click(click(cx, move |view, window, cx| {
                    view.prepare_command(command, args, cx);
                    view.composer_focus.focus(window, cx);
                    cx.notify();
                })),
            );
        }
        panel = panel.child(actions);
        let source = match self.extension_kind {
            "skills" => Some("skill"),
            "templates" => Some("template"),
            "agents" => Some("agent"),
            _ => None,
        };
        if let Some(source) = source {
            let mut commands = div().flex().flex_wrap().gap_1();
            for command in session
                .controls
                .as_ref()
                .into_iter()
                .flat_map(|controls| &controls.commands)
                .filter(|command| command.source == source)
            {
                let name = command.name.clone();
                let args = !command.args.is_empty();
                let form = command
                    .parameters
                    .as_ref()
                    .is_some_and(|parameters| !parameters.is_empty());
                commands = commands.child(
                    Button::new(
                        SharedString::from(format!("extension-use-{name}")),
                        name.clone(),
                    )
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, window, cx| {
                        if form {
                            view.extension_template = Some(name.clone());
                        } else {
                            view.prepare_command(&name, args, cx);
                            view.composer_focus.focus(window, cx);
                        }
                        cx.notify();
                    })),
                );
            }
            panel = panel.child(commands);
        }
        if self.extension_kind == "templates"
            && let Some(command) = session
                .controls
                .as_ref()
                .into_iter()
                .flat_map(|catalog| &catalog.commands)
                .find(|command| {
                    Some(&command.name) == self.extension_template.as_ref()
                        && command.source == "template"
                })
            && let Some(parameters) = &command.parameters
        {
            let mut form = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(format!("{} · {}", command.name, command.description));
            let mut values = HashMap::new();
            for name in parameters {
                let key = format!(
                    "template:{}:{}:{}:{name}",
                    session.id, session.generation, command.name
                );
                let value = self.fields.get(&key).cloned().unwrap_or_default();
                values.insert(name.clone(), value.clone());
                let weak = cx.entity().downgrade();
                form = form.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .w(px(100.0))
                                .flex_shrink_0()
                                .text_ellipsis()
                                .child(name.clone()),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(SharedString::from(key.clone()))
                                    .value(value)
                                    .placeholder(name.clone())
                                    .disabled(self.remote_read_only(&session.id))
                                    .on_text_change(move |text, _, cx| {
                                        let _ = weak.update(cx, |view, cx| {
                                            view.fields.insert(key.clone(), text.into());
                                            cx.notify();
                                        });
                                    }),
                            ),
                        ),
                );
            }
            let prepared = command.template_command(&values);
            form = form
                .child(
                    Button::new("prepare-template", "Prepare template")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Xs)
                        .disabled(prepared.is_none() || self.remote_read_only(&session.id))
                        .on_click(click(cx, move |view, window, cx| {
                            if let Some(command) = &prepared {
                                view.prepare_command(command, false, cx);
                                view.composer_focus.focus(window, cx);
                                cx.notify();
                            }
                        })),
                )
                .child("Single-line values, up to 4096 bytes each; blank fields send empty values. Prepare, review, then Send.");
            panel = panel.child(form);
        }
        if let Some(inventory) = inventory {
            panel = panel.child(Markdown::new("extension-report", inventory.report.clone()));
        }
        panel.into_any_element()
    }

    fn history_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let mut panel = div()
            .id("conversation-history-controls")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs();
        let mut tabs = div().flex().gap_1();
        for (id, label, checkpoints) in [
            ("history-checkpoints", "Checkpoints", true),
            ("history-forks", "Fork points", false),
        ] {
            tabs = tabs.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(if self.history_checkpoints == checkpoints {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.history_checkpoints = checkpoints;
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(tabs);
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| {
                if self.history_checkpoints {
                    telemetry.checkpoints.as_ref()
                } else {
                    telemetry.branches.as_ref()
                }
            });
        let page = snapshot.map_or(1, |snapshot| snapshot.page);
        let pages = snapshot.map_or(1, |snapshot| snapshot.total.div_ceil(32).max(1));
        let command = if self.history_checkpoints {
            "/checkpoint"
        } else {
            "/tree"
        };
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, text, args, disabled) in [
            (
                "history-load",
                "Load / refresh",
                format!("{command} --page {page}"),
                false,
                false,
            ),
            (
                "history-prev",
                "‹",
                format!("{command} --page {}", page.saturating_sub(1)),
                false,
                page <= 1,
            ),
            (
                "history-next",
                "›",
                format!("{command} --page {}", page.saturating_add(1)),
                false,
                page >= pages,
            ),
            (
                "history-new",
                "New checkpoint…",
                "/checkpoint".to_owned(),
                true,
                false,
            ),
            (
                "history-clone",
                "Clone branch",
                "/clone".to_owned(),
                false,
                false,
            ),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&text, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        if !self.history_checkpoints {
            actions = actions.child(
                Button::new("history-tree", "Tree")
                    .size(ButtonSize::Xs)
                    .variant(if self.history_tree {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .disabled(snapshot.is_none_or(|snapshot| snapshot.report.is_empty()))
                    .on_click(click(cx, |view, _, cx| {
                        view.history_tree = !view.history_tree;
                        cx.notify();
                    })),
            );
        }
        panel = panel
            .child(actions)
            .child("Conversation history · Prepare an action, then Send");
        let Some(snapshot) = snapshot else {
            return panel.into_any_element();
        };
        panel = panel.child(format!("{} points · Page {page}/{pages}", snapshot.total));
        if self.history_tree && !snapshot.report.is_empty() {
            panel = panel.child(Markdown::new(
                "conversation-tree-report",
                format!("```text\n{}\n```", snapshot.report),
            ));
        }
        for (index, row) in snapshot.rows.iter().enumerate() {
            let command = row.command(&snapshot.kind);
            panel = panel.child(
                div()
                    .py_1()
                    .child(div().text_ellipsis().child(row.label.clone()))
                    .child(div().text_ellipsis().child(row.detail.clone()))
                    .child(
                        Button::new(
                            format!("history-point-{index}"),
                            if self.history_checkpoints {
                                "Roll back"
                            } else {
                                "Fork here"
                            },
                        )
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(command.is_none())
                        .on_click(click(cx, move |view, window, cx| {
                            if let Some(command) = &command {
                                view.prepare_command(command, false, cx);
                                view.composer_focus.focus(window, cx);
                                cx.notify();
                            }
                        })),
                    ),
            );
        }
        panel.into_any_element()
    }

    fn config_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let config = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.config.as_ref());
        let mut panel = div()
            .id("engine-config")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs();
        let page = config.map_or(1, |config| config.page);
        let pages = config.map_or(1, |config| config.total.div_ceil(32).max(1));
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, disabled) in [
            (
                "config-load",
                "Load / refresh",
                format!("/config --page {page}"),
                false,
            ),
            (
                "config-prev",
                "‹",
                format!("/config --page {}", page.saturating_sub(1)),
                page <= 1,
            ),
            (
                "config-next",
                "›",
                format!("/config --page {}", page.saturating_add(1)),
                page >= pages,
            ),
            (
                "config-resolved",
                "Resolved",
                "/config --resolved".to_owned(),
                false,
            ),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Xs)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&command, false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(actions);
        let Some(config) = config else {
            return panel
                .child("Load requests settings from this engine. Send to confirm.")
                .into_any_element();
        };
        panel = panel.child(format!(
            "{} settings · Page {page}/{pages} · Prepare, then Send to save",
            config.total
        ));
        for field in &config.fields {
            let key = format!("config-edit-{}-{}", session.id, field.path);
            let value = self
                .fields
                .get(&key)
                .cloned()
                .unwrap_or_else(|| field.value.clone());
            let weak = cx.entity().downgrade();
            let edit_key = key.clone();
            let input = Input::new(SharedString::from(key.clone()))
                .size(gpui_ui_kit::InputSize::Sm)
                .value(value.clone())
                .placeholder(field.hint.clone())
                .disabled(!field.editable)
                .on_text_change(move |text, _, cx| {
                    let _ = weak.update(cx, |view, cx| {
                        view.fields.insert(edit_key.clone(), text.into());
                        cx.notify();
                    });
                });
            let command = field.command(&value);
            panel = panel.child(
                div()
                    .py_1()
                    .child(
                        div()
                            .text_ellipsis()
                            .child(format!("{} · {}", field.path, field.label)),
                    )
                    .child(div().text_ellipsis().child(format!(
                        "Current: {}",
                        if field.value.is_empty() {
                            "unset"
                        } else {
                            &field.value
                        }
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap_1()
                            .child(div().w(px(180.0)).child(input))
                            .child(
                                Button::new(format!("config-prepare-{}", field.path), "Prepare")
                                    .size(ButtonSize::Xs)
                                    .variant(ButtonVariant::Ghost)
                                    .disabled(command.is_none())
                                    .on_click(click(cx, move |view, window, cx| {
                                        if let Some(command) = &command {
                                            view.prepare_command(command, false, cx);
                                            view.fields.remove(&key);
                                            view.composer_focus.focus(window, cx);
                                            cx.notify();
                                        }
                                    })),
                            ),
                    ),
            );
        }
        panel.into_any_element()
    }

    fn saved_sessions_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.saved_sessions.as_ref());
        let mut panel = div()
            .id("saved-conversations")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs();
        let mut tabs = div().flex().gap_1();
        for (id, label, current) in [
            ("saved-browse-view", "Browse", false),
            ("saved-current-view", "Current", true),
        ] {
            tabs = tabs.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(if self.saved_current == current {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.saved_current = current;
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(tabs);
        if self.saved_current {
            return panel
                .child(self.current_conversation_controls(session, cx))
                .into_any_element();
        }
        let mut actions = div().flex().flex_wrap().gap_1();
        let page = snapshot.map_or(1, |snapshot| snapshot.page);
        for (id, label, command, args) in [
            (
                "saved-refresh",
                "Load / refresh",
                format!("/list {page}"),
                false,
            ),
            ("saved-search", "Search…", "/search".to_owned(), true),
            ("saved-save", "Save current", "/save".to_owned(), false),
            (
                "saved-retitle",
                "Rebuild all titles",
                "/retitle".to_owned(),
                false,
            ),
        ] {
            let supported = session.controls.as_ref().is_none_or(|catalog| {
                catalog
                    .commands
                    .iter()
                    .any(|entry| Some(entry.name.as_str()) == command.split_whitespace().next())
            });
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!supported || self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&command, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(actions);
        let Some(snapshot) = snapshot else {
            return panel
                .child("Load requests saved conversations from this engine. Send to confirm.")
                .into_any_element();
        };
        let pages = snapshot.total.div_ceil(64).max(1);
        let mut navigation = div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(format!("{} saved · Page {page}/{pages}", snapshot.total));
        for (id, label, target, disabled) in [
            ("saved-prev", "‹", page.saturating_sub(1), page <= 1),
            ("saved-next", "›", page.saturating_add(1), page >= pages),
        ] {
            navigation = navigation.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&format!("/list {target}"), false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(navigation);
        for saved in &snapshot.sessions {
            let mut row = div()
                .py_1()
                .child(
                    div()
                        .text_ellipsis()
                        .child(format!("{} · {}", saved.id, saved.title)),
                )
                .child(
                    div()
                        .text_ellipsis()
                        .child(format!("{} · {}", saved.tag, saved.cwd)),
                );
            let mut buttons = div().flex().gap_1();
            for (action, label) in [("resume", "Resume"), ("del", "Delete")] {
                let command = format!("/{action} {}", saved.id);
                buttons = buttons.child(
                    Button::new(format!("saved-{action}-{}", saved.id), label)
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(self.remote_read_only(&session.id))
                        .on_click(click(cx, move |view, window, cx| {
                            view.prepare_command(&command, false, cx);
                            view.composer_focus.focus(window, cx);
                            cx.notify();
                        })),
                );
            }
            row = row.child(buttons);
            panel = panel.child(row);
        }
        panel.into_any_element()
    }

    fn export_download_row(
        &self,
        session: &Session,
        export: &crate::model::EngineExport,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let mut panel = div().flex().flex_col().gap_1();
        let key = export_key(&session.id, session.generation, &export.id);
        let busy = self.export_jobs.contains(&key);
        let id = session.id.clone();
        let generation = session.generation;
        let artifact = export.clone();
        #[cfg(target_family = "wasm")]
        let ready = self.browser_exports.contains_key(&key);
        #[cfg(not(target_family = "wasm"))]
        let ready = false;
        let click_key = key.clone();
        panel = panel.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_1()
                .text_xs()
                .child(format!("{} · {} bytes", export.name, export.bytes))
                .child(
                    Button::new(
                        "current-export-download",
                        if busy {
                            "Downloading…"
                        } else if ready {
                            "Save file"
                        } else {
                            "Download"
                        },
                    )
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(busy || !self.connected)
                    .on_click(click(cx, move |view, _, cx| {
                        #[cfg(target_family = "wasm")]
                        if let Some((url, name)) = view.browser_exports.get(&click_key) {
                            if let Err(error) = crate::web::save_file(url, name) {
                                view.export_feedback = Some((click_key.clone(), error.to_string()));
                            }
                            cx.notify();
                            return;
                        }
                        #[cfg(not(target_family = "wasm"))]
                        let _ = &click_key;
                        view.download_export(id.clone(), generation, artifact.clone(), cx);
                    })),
                ),
        );
        if let Some((id, message)) = &self.export_feedback
            && id == &key
        {
            panel = panel.child(div().text_xs().child(message.clone()));
        }
        panel.into_any_element()
    }

    fn current_conversation_controls(
        &self,
        session: &Session,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let mut panel = div().flex().flex_col().gap_1();
        panel = panel.child("Current Plank conversation · choose an action, then Send.");
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, args) in [
            ("current-save", "Save", "/save", false),
            ("current-rename", "Rename…", "/rename", true),
            ("current-tag", "Tag…", "/tag", true),
            ("current-tag-clear", "Clear tag", "/tag -", false),
            ("current-export-md", "Export Markdown", "/export md", false),
            ("current-export-html", "Export HTML", "/export html", false),
        ] {
            let supported = session.controls.as_ref().is_none_or(|catalog| {
                catalog
                    .commands
                    .iter()
                    .any(|entry| Some(entry.name.as_str()) == command.split_whitespace().next())
            });
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!supported || self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(actions);
        if let Some(export) = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.export.as_ref())
            && export.is_valid()
        {
            panel = panel.child(self.export_download_row(session, export, cx));
        }
        let mut ratings = div().flex().flex_wrap().gap_1().child("Last turn:");
        for (id, label, command) in [
            ("current-rate-positive", "Helpful + note…", "/rate +"),
            ("current-rate-negative", "Unhelpful + note…", "/rate -"),
        ] {
            let supported = session
                .controls
                .as_ref()
                .is_none_or(|catalog| catalog.commands.iter().any(|entry| entry.name == "/rate"));
            ratings = ratings.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!supported || self.remote_read_only(&session.id))
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, true, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel.child(ratings)
            .child("Rename keeps the old saved copy. Exports are written on the engine host; received exports also offer a client download.")
            .into_any_element()
    }

    fn model_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let mut tabs = div().flex().flex_wrap().gap_1();
        for (id, label, model_view) in [
            ("model-settings-view", "Settings", "settings"),
            ("model-downloads-view", "Downloads", "downloads"),
            ("model-engines-view", "Engines", "engines"),
            ("model-metrics-view", "Metrics", "metrics"),
            ("model-remote-view", "Remote", "remote"),
        ] {
            tabs = tabs.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(if self.model_view == model_view {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.model_view = model_view;
                        cx.notify();
                    })),
            );
        }
        if self.model_view == "remote" {
            return div()
                .id("remote-host-controls")
                .max_h(px(160.0))
                .overflow_y_scroll()
                .text_xs()
                .child(tabs)
                .child(self.host_panel(session, cx))
                .into_any_element();
        }
        if self.model_view == "engines" {
            return div()
                .id("engine-inventory-controls")
                .max_h(px(160.0))
                .overflow_y_scroll()
                .text_xs()
                .child(tabs)
                .child(self.engines_panel(session, cx))
                .into_any_element();
        }
        if self.model_view == "metrics" {
            return div()
                .id("model-metrics-controls")
                .max_h(px(160.0))
                .overflow_y_scroll()
                .text_xs()
                .child(tabs)
                .child(self.metrics_panel(session, cx))
                .into_any_element();
        }
        if self.model_view == "downloads" {
            return div()
                .id("model-download-controls")
                .max_h(px(160.0))
                .overflow_y_scroll()
                .text_xs()
                .child(tabs)
                .child(self.downloads_panel(session, cx))
                .into_any_element();
        }
        let settings = session
            .controls
            .as_ref()
            .and_then(|controls| controls.settings.as_ref());
        let reasoning = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.status.as_ref())
            .map(|status| format!("Reasoning: {}", status.think))
            .unwrap_or_else(|| "Reasoning: unavailable".to_owned());
        let mut panel = div()
            .id("model-controls")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .text_xs()
            .child(tabs)
            .child(reasoning)
            .child("Choose a value, then Send. Availability depends on the engine.");
        for (name, command, values) in [
            ("Reasoning", "/think", &["off", "low", "medium", "max"][..]),
            ("Temperature", "/temp", &["0", "0.5", "1"][..]),
            ("GPU power %", "/power", &["25", "50", "75", "100"][..]),
            ("Speculative decoding", "/mtp", &["on", "off"][..]),
            ("Micro-compaction", "/mc", &["on", "off"][..]),
            ("Loop guard", "/loopguard", &["on", "off"][..]),
        ] {
            let current = settings.map(|settings| match command {
                "/temp" => settings.temperature.to_string(),
                "/power" => {
                    if settings.power_percent == 0 {
                        "default".to_owned()
                    } else {
                        settings.power_percent.to_string()
                    }
                }
                "/mtp" => if settings.speculative { "on" } else { "off" }.to_owned(),
                "/mc" => if settings.micro_compaction {
                    "on"
                } else {
                    "off"
                }
                .to_owned(),
                "/loopguard" => if settings.loop_guard { "on" } else { "off" }.to_owned(),
                _ => String::new(),
            });
            let label = if command == "/think" {
                name.to_owned()
            } else {
                format!("{name}: {}", current.as_deref().unwrap_or("unavailable"))
            };
            let mut row = div().flex().flex_wrap().items_center().gap_1().child(label);
            for value in values {
                let prepared = format!("{command} {value}");
                row = row.child(
                    Button::new(format!("model-{command}-{value}"), *value)
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(
                            command == "/mtp"
                                && *value == "on"
                                && settings.is_some_and(|settings| !settings.speculation_available),
                        )
                        .on_click(click(cx, move |view, window, cx| {
                            view.prepare_command(&prepared, false, cx);
                            view.composer_focus.focus(window, cx);
                            cx.notify();
                        })),
                );
            }
            row = row.child(
                Button::new(format!("model-{command}-custom"), "Custom…")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, true, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
            panel = panel.child(row);
        }
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, args) in [
            ("model-engines", "Engines", "/engines", false),
            ("model-steer", "Steering…", "/steer", true),
            ("model-context", "Context", "/context", false),
            ("model-usage", "Usage", "/usage", false),
            ("model-compact", "Compact…", "/compact", true),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, args, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        if let Some(settings) = settings {
            panel = panel.child(format!(
                "Model: {} · Steering: {}",
                settings.model,
                settings
                    .steering
                    .map_or_else(|| "unavailable".to_owned(), |scale| scale.to_string())
            ));
        }
        panel.child(actions).into_any_element()
    }

    fn host_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.host.as_ref());
        let remote_session = matches!(session.backend, Backend::Remote { .. });
        let mut actions = div().flex().flex_wrap().gap_1();
        let access = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.remote_access.as_ref());
        if remote_session {
            for (label, command, disabled) in [
                (
                    "Connect as observer",
                    Command::ConnectRemote {
                        session_id: session.id.clone(),
                    },
                    access.is_some() || session.state.busy() || session.recovery_required,
                ),
                (
                    "Request control",
                    Command::RemoteControl {
                        session_id: session.id.clone(),
                        request: true,
                    },
                    access.is_none_or(|access| access.controller || access.requesting)
                        || session.recovery_required,
                ),
                (
                    "Release control",
                    Command::RemoteControl {
                        session_id: session.id.clone(),
                        request: false,
                    },
                    access.is_none_or(|access| !access.controller) || session.state.busy(),
                ),
            ] {
                actions = actions.child(
                    Button::new(format!("remote-{label}"), label)
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(disabled)
                        .on_click(click(cx, move |view, _, cx| {
                            view.command(command.clone());
                            cx.notify();
                        })),
                );
            }
        }
        for (id, label, command, disabled) in [
            ("host-status", "Report", "/rc status", false),
            (
                "host-auto",
                "Start · automatic grants",
                "/rc on",
                !remote_session,
            ),
            (
                "host-ask",
                "Start · require grants",
                "/rc ask",
                !remote_session,
            ),
            (
                "host-off",
                "Stop host",
                "/rc off",
                !remote_session || snapshot.is_some_and(|host| host.holder == "off"),
            ),
            (
                "host-grant",
                "Grant next",
                "/grant",
                snapshot.is_none_or(|host| host.pending.is_empty())
                    || self.remote_read_only(&session.id),
            ),
            ("host-engine", "Remote engine", "/remote", false),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        let mut panel = div().text_xs().child(actions);
        if let Some(access) = access {
            panel = panel.child(format!(
                "This attachment: {} · client {}{}",
                if access.controller {
                    "controller"
                } else {
                    "observer"
                },
                access
                    .client_id
                    .map_or_else(|| "unknown".into(), |id| id.to_string()),
                if access.waiting_input {
                    " · input held until granted"
                } else if access.requesting {
                    " · requesting control"
                } else {
                    ""
                }
            ));
            if let Some(reason) = &access.reason {
                panel = panel.child(reason.clone());
            }
        }
        if !remote_session {
            panel = panel.child("Local GUI hosting needs the console bridge. Existing remote Plank hosts can be managed here.");
        }
        if let Some(host) = snapshot {
            if let Some(endpoint) = &host.endpoint {
                panel = panel.child(format!(
                    "Engine-host endpoint: {endpoint} · use an SSH tunnel from another machine"
                ));
            }
            let owner = match host.holder.as_str() {
                "remote" => format!(
                    "Remote client {}{}",
                    host.controller.unwrap_or_default(),
                    if host.reserved {
                        " · reconnect slot reserved"
                    } else {
                        ""
                    }
                ),
                "local" => "Local operator".into(),
                "free" => "Available".into(),
                _ => "Host stopped".into(),
            };
            panel = panel.child(format!("Control: {owner}"));
            if host.endpoint.is_some() {
                panel = panel.child(if host.automatic {
                    "Clients may take control from the local operator automatically."
                } else {
                    "Taking control from the local operator requires an explicit grant."
                });
            }
            panel = panel.child(format!("{} waiting for control", host.pending_total));
            for id in &host.pending {
                let command = format!("/grant {id}");
                panel = panel.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(format!("Client {id}"))
                        .child(
                            Button::new(format!("grant-client-{id}"), "Grant…")
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::Ghost)
                                .disabled(self.remote_read_only(&session.id))
                                .on_click(click(cx, move |view, window, cx| {
                                    view.prepare_command(&command, false, cx);
                                    view.composer_focus.focus(window, cx);
                                    cx.notify();
                                })),
                        ),
                );
            }
            if host.pending_total > host.pending.len() as u64 {
                panel = panel
                    .child("First 64 waiting clients shown; Grant next serves the oldest request.");
            }
        } else {
            panel = panel
                .child("Send Report for host status. This view requires a rebuilt Plank host.");
        }
        panel.child("Grant hands away control and clears all waiting requests. Actions require Send; changing policy requires stopping the existing host first.").into_any_element()
    }

    fn remote_read_only(&self, id: &str) -> bool {
        self.snapshot
            .sessions
            .iter()
            .find(|session| session.id == id)
            .is_some_and(|session| matches!(session.backend, Backend::Remote { .. }))
            && self
                .snapshot
                .telemetry
                .get(id)
                .and_then(|telemetry| telemetry.remote_access.as_ref())
                .is_none_or(|access| !access.controller)
    }

    fn engines_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.metrics.get("engines"))
            .and_then(|metrics| match metrics {
                crate::model::EngineMetrics::Engines { snapshot } => Some(snapshot),
                _ => None,
            });
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, disabled) in [
            (
                "engine-inventory-refresh",
                "Load / refresh",
                "/engines --page 1",
                !session.controls.as_ref().is_some_and(|controls| {
                    controls.commands.iter().any(|command| {
                        command.name == "/engines" && command.args.contains("--page")
                    })
                }),
            ),
            (
                "engine-inventory-choose",
                "Choose…",
                "/engines",
                !matches!(session.backend, Backend::Local { .. })
                    || self.remote_read_only(&session.id),
            ),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        let mut panel = div().child(actions);
        let Some(snapshot) = snapshot else {
            return panel.child("Load, then Send to inspect this engine host's catalog. Older engines retain text reports.").into_any_element();
        };
        panel = panel
            .child(format!(
                "{} engines · page {}/{} · current {}",
                snapshot.total,
                snapshot.page,
                snapshot.total.div_ceil(32).max(1),
                snapshot
                    .current
                    .as_deref()
                    .unwrap_or("custom path / provider")
            ))
            .child(format!(
                "Host build: DeepSeek / Qwen {} · Gemma {}",
                if snapshot.ds4 {
                    "available"
                } else {
                    "unavailable"
                },
                if snapshot.gemma {
                    "available"
                } else {
                    "unavailable"
                }
            ));
        for (label, bytes) in [
            ("RAM", snapshot.ram),
            ("Free download space", snapshot.free_disk),
        ] {
            if let Some(bytes) = bytes {
                panel = panel.child(format!(
                    "{label}: {:.1} GiB",
                    bytes as f64 / 1_073_741_824.0
                ));
            }
        }
        let mut navigation = div().flex().gap_1();
        for (label, target, disabled) in [
            ("‹", snapshot.page.saturating_sub(1), snapshot.page <= 1),
            (
                "›",
                snapshot.page + 1,
                snapshot.page >= snapshot.total.div_ceil(32).max(1),
            ),
        ] {
            navigation = navigation.child(
                Button::new(format!("engine-page-{label}"), label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(&format!("/engines --page {target}"), false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(navigation);
        for engine in &snapshot.engines {
            panel = panel.child(
                div()
                    .py_1()
                    .child(format!(
                        "{} · v{} · {}",
                        engine.name, engine.version, engine.detail
                    ))
                    .child(engine.notes.clone()),
            );
        }
        panel.child("Snapshot at request time. Choose opens the local engine chooser after Send; remote restart is not available yet.").into_any_element()
    }

    fn metrics_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        use crate::model::EngineMetrics;
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let mut tabs = div().flex().flex_wrap().gap_1();
        for (kind, label) in [
            ("context", "Context"),
            ("usage", "Usage"),
            ("throughput", "Speed"),
            ("activity", "Activity"),
            ("insights", "Insights"),
        ] {
            tabs = tabs.child(
                Button::new(format!("metrics-{kind}"), label)
                    .size(ButtonSize::Xs)
                    .variant(if self.metrics_kind == kind {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        view.metrics_kind = kind;
                        cx.notify();
                    })),
            );
        }
        let command = match self.metrics_kind {
            "usage" => "/usage",
            "throughput" => "/toks",
            "activity" => "/stats",
            "insights" => "/insights fast",
            _ => "/context",
        };
        tabs = tabs.child(
            Button::new("metrics-refresh", "Load / refresh")
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .on_click(click(cx, move |view, window, cx| {
                    view.prepare_command(command, false, cx);
                    view.composer_focus.focus(window, cx);
                    cx.notify();
                })),
        );
        let mut panel = div().text_xs().child(tabs);
        if self.metrics_kind == "insights" {
            let mut actions = div().flex().flex_wrap().gap_1();
            for (id, label, command) in [
                ("insights-full", "Written insights", "/insights"),
                ("insights-fresh", "Rewrite insights", "/insights fresh"),
            ] {
                actions = actions.child(
                    Button::new(id, label)
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .disabled(self.remote_read_only(&session.id))
                        .on_click(click(cx, move |view, window, cx| {
                            view.prepare_command(command, false, cx);
                            view.composer_focus.focus(window, cx);
                            cx.notify();
                        })),
                );
            }
            panel = panel.child(actions).child("Load scans saved conversations without model writing. Written insights may reuse prose; Rewrite regenerates it.");
            if let Some(export) = self
                .snapshot
                .telemetry
                .get(&session.id)
                .and_then(|telemetry| telemetry.insights_export.as_ref())
                && export.is_valid()
                && self
                    .snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.metrics.get("insights"))
                    .is_none_or(|metrics| export.matches_insights(metrics))
            {
                panel = panel.child(self.export_download_row(session, export, cx));
            }
        }
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.metrics.get(self.metrics_kind));
        match snapshot {
            Some(EngineMetrics::Insights { markdown, .. }) => {
                panel = panel.child(self.insights_report(markdown, cx));
            }
            Some(EngineMetrics::Engines { .. }) => {}
            Some(EngineMetrics::Subagents { .. }) => {}
            Some(EngineMetrics::Activity { snapshot }) => {
                let mut scopes = div().flex().gap_1();
                for (scope, label) in [("all", "All time"), ("7", "7 days"), ("30", "30 days")] {
                    scopes = scopes.child(
                        Button::new(format!("activity-scope-{scope}"), label)
                            .size(ButtonSize::Xs)
                            .variant(if snapshot.scope == scope {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Ghost
                            })
                            .on_click(click(cx, move |view, window, cx| {
                                view.prepare_command(&format!("/stats {scope}"), false, cx);
                                view.composer_focus.focus(window, cx);
                                cx.notify();
                            })),
                    );
                }
                let mut calendar = div().flex().gap(px(1.0));
                for week in snapshot.days.chunks(7) {
                    let mut column = div().flex().flex_col().gap(px(1.0)).flex_shrink_0();
                    for day in week {
                        let color = match day.level {
                            Some(1) => gpui::rgb(0x275b3d),
                            Some(2) => gpui::rgb(0x368a50),
                            Some(3) => gpui::rgb(0x55bd6b),
                            Some(4) => gpui::rgb(0x8bdf8f),
                            _ => colors.muted,
                        };
                        let date = day.date.clone();
                        column = column.child(
                            div()
                                .id(SharedString::from(format!("activity-day-{date}")))
                                .w(px(6.0))
                                .h(px(6.0))
                                .bg(color)
                                .opacity(if day.level.is_none() {
                                    0.0
                                } else if day.level == Some(0) {
                                    0.2
                                } else {
                                    1.0
                                })
                                .cursor_pointer()
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.fields.insert("activity-day".into(), date.clone());
                                    cx.notify();
                                })),
                        );
                    }
                    calendar = calendar.child(column);
                }
                panel = panel
                    .child(scopes)
                    .child(
                        div()
                            .id("activity-calendar")
                            .overflow_x_scroll()
                            .child(calendar),
                    )
                    .child("Mon → Sun · oldest → newest · four ranked green tones");
                if let Some(day) = self
                    .fields
                    .get("activity-day")
                    .and_then(|date| snapshot.days.iter().find(|day| &day.date == date))
                {
                    panel = panel.child(if day.level.is_some() {
                        format!("{} · {} approximate tokens", day.date, day.tokens)
                    } else {
                        format!("{} · future day", day.date)
                    });
                } else if let (Some(first), Some(last)) =
                    (snapshot.days.first(), snapshot.days.last())
                {
                    panel = panel.child(format!(
                        "{} – {} · click a day for details",
                        first.date, last.date
                    ));
                }
                for pair in snapshot.summary.chunks(2) {
                    let mut row = div().flex().flex_wrap().gap_2();
                    for line in pair {
                        row = row.child(div().flex_1().child(line.clone()));
                    }
                    panel = panel.child(row);
                }
                panel = panel.child("Saved-session activity · host-local dates · tokens estimated from transcript bytes");
            }
            Some(EngineMetrics::Context {
                model,
                window,
                used,
                categories,
            }) => {
                let (used, window) = self
                    .snapshot
                    .telemetry
                    .get(&session.id)
                    .and_then(|telemetry| telemetry.status.as_ref())
                    .filter(|status| status.ctx_size > 0)
                    .map_or((used, window), |status| {
                        (&status.ctx_used, &status.ctx_size)
                    });
                panel = panel
                    .child(model.clone())
                    .child(format!("Resident context: {used}/{window} tokens"));
                if *window > 0 {
                    panel = panel.child(
                        Progress::new((*used as f64 / *window as f64).min(1.0) as f32)
                            .size(ProgressSize::Xs)
                            .show_label(true)
                            .aria_label("Resident context fill"),
                    );
                }
                panel =
                    panel.child("Live resident usage · category estimates refresh between passes");
                for (label, count) in ["System", "MCP tools", "Messages", "AGENTS.md", "Memory"]
                    .iter()
                    .zip(categories)
                {
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().w(px(110.0)).child(format!("{label}: {count}")));
                    if *window > 0 {
                        row = row.child(
                            div().flex_1().child(
                                Progress::new((*count as f64 / *window as f64).min(1.0) as f32)
                                    .size(ProgressSize::Xs)
                                    .aria_label(format!("{label} share of context window")),
                            ),
                        );
                    }
                    panel = panel.child(row);
                }
                panel = panel.child(
                    Button::new("metrics-compact", "Compact…")
                        .size(ButtonSize::Xs)
                        .variant(ButtonVariant::Ghost)
                        .on_click(click(cx, |view, window, cx| {
                            view.prepare_command("/compact", true, cx);
                            view.composer_focus.focus(window, cx);
                            cx.notify();
                        })),
                );
            }
            Some(EngineMetrics::Usage {
                model,
                input,
                output,
                cache_read,
                cache_write,
                provider_passes,
                wall_secs,
            }) => {
                panel = panel
                    .child(model.clone())
                    .child(format!(
                        "Input: {input} · Output: {output} · Total: {}",
                        input.saturating_add(*output)
                    ))
                    .child(format!(
                        "Cache read: {cache_read} · Cache write: {cache_write}"
                    ))
                    .child(format!("Session elapsed: {:.1} min", wall_secs / 60.0));
                if let Some(passes) = provider_passes {
                    panel = panel.child(format!("Provider-reported passes: {passes}"));
                }
                panel = panel.child("Input includes cached provider tokens. Local input is estimated from context growth; totals include alternate engines.");
            }
            Some(EngineMetrics::Throughput {
                generation,
                prefill,
            }) => {
                for (label, samples) in [
                    ("Decode · one-second windows", generation),
                    ("Prefill · completed passes", prefill),
                ] {
                    if samples.is_empty() {
                        panel = panel.child(format!("{label}: no samples"));
                        continue;
                    }
                    let peak = samples.iter().copied().fold(0.0_f64, f64::max);
                    panel = panel.child(format!(
                        "{label} · latest {:.1} · peak {peak:.1} tok/s",
                        samples.last().unwrap()
                    ));
                    let mut chart = div().flex().items_end().h(px(36.0)).gap(px(1.0));
                    for rate in samples {
                        chart = chart.child(
                            div()
                                .flex_1()
                                .h(px(if peak > 0.0 {
                                    (rate / peak * 36.0) as f32
                                } else {
                                    0.0
                                }))
                                .bg(colors.accent),
                        );
                    }
                    panel = panel.child(chart);
                }
                panel = panel
                    .child("Oldest → newest · independent vertical scales · process-wide history");
            }
            None => {
                panel = panel.child(
                    "Load, then Send to fetch measurements. Older engines retain text reports.",
                )
            }
        }
        panel
            .child("Snapshot at last request; refresh prepares a command for Send.")
            .into_any_element()
    }

    fn insights_report(&self, source: &str, cx: &Context<Self>) -> gpui::AnyElement {
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let mut report = div().flex().flex_col().gap_1().text_xs();
        for block in crate::report::parse(source) {
            report = match block {
                crate::report::Block::Heading(text) => {
                    report.child(div().font_weight(gpui::FontWeight::BOLD).child(text))
                }
                crate::report::Block::Text(text) => report.child(div().child(text)),
                crate::report::Block::Code(text) => {
                    report.child(div().bg(colors.surface).p_1().child(text))
                }
                crate::report::Block::Table(rows) => {
                    let mut table = div().flex().flex_col().gap_1();
                    for (index, cells) in rows.into_iter().enumerate() {
                        let mut row = div().flex().gap_2();
                        if index == 0 {
                            row = row.font_weight(gpui::FontWeight::BOLD);
                        }
                        for (column, text) in cells.into_iter().enumerate() {
                            row = row.child(if column == 0 {
                                div().flex_1().min_w_0().child(text)
                            } else {
                                div().w(px(70.0)).flex_shrink_0().child(text)
                            });
                        }
                        table = table.child(row);
                    }
                    report.child(table)
                }
            };
        }
        report.into_any_element()
    }

    fn downloads_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let snapshot = self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.downloads.as_ref());
        let mut panel = div().text_xs();
        let mut actions = div().flex().flex_wrap().gap_1();
        for (id, label, command, disabled) in [
            ("download-report", "Report", "/model status", false),
            (
                "download-start",
                "Download / resume",
                "/model download",
                snapshot.is_some_and(|snapshot| {
                    !snapshot.pending
                        || snapshot
                            .state
                            .as_ref()
                            .is_some_and(crate::model::ModelDownload::can_cancel)
                }),
            ),
            (
                "download-stop",
                "Stop · keep partials",
                "/model cancel",
                snapshot.is_some_and(|snapshot| {
                    snapshot
                        .state
                        .as_ref()
                        .is_none_or(|state| !state.can_cancel())
                }),
            ),
            (
                "download-delete",
                "Stop · delete partials",
                "/model cancel --delete",
                snapshot.is_some_and(|snapshot| {
                    snapshot
                        .state
                        .as_ref()
                        .is_none_or(|state| !state.can_cancel())
                }),
            ),
            ("download-engines", "Engines…", "/engines", false),
        ] {
            actions = actions.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, window, cx| {
                        view.prepare_command(command, false, cx);
                        view.composer_focus.focus(window, cx);
                        cx.notify();
                    })),
            );
        }
        panel = panel.child(actions);
        if let Some(progress) = &session.progress {
            panel = panel.child(format!("Engine setup · {}", progress.label));
            if progress.total > 0 {
                panel = panel.child(
                    Progress::new(
                        (progress.current.min(progress.total) as f64 / progress.total as f64)
                            as f32,
                    )
                    .size(ProgressSize::Xs)
                    .show_label(true)
                    .aria_label("Engine setup progress"),
                );
            }
        }
        let Some(snapshot) = snapshot else {
            return panel.child("Helper status is unavailable from this engine. Send Report for its text output.").into_any_element();
        };
        if let Some(state) = &snapshot.state {
            let phase = match state.phase.as_str() {
                "rehashing" => "Resuming · reading partial file",
                "downloading" => "Downloading",
                "verifying" => "Verifying checksum",
                "staged" => "Verified · ready on next engine start",
                "failed" => "Failed",
                "cancelled" => "Cancelled",
                _ => "Unrecognized helper state",
            };
            panel = panel
                .child(format!(
                    "{} · manifest {} · {phase}",
                    state.set, state.version
                ))
                .child(format!(
                    "{} · artifact {}/{}",
                    state.current, state.index, state.of
                ));
            if let Some(fraction) = state.fraction() {
                panel = panel
                    .child(
                        Progress::new(fraction)
                            .size(ProgressSize::Xs)
                            .show_label(true)
                            .aria_label("Current model artifact download"),
                    )
                    .child(format!(
                        "{:.1}/{:.1} GiB · {:.1} MiB/s",
                        state.done_bytes as f64 / 1_073_741_824.0,
                        state.total_bytes as f64 / 1_073_741_824.0,
                        state.rate_bps as f64 / 1_048_576.0
                    ));
            }
            if (state.phase == "failed" || state.phase == "unknown")
                && let Some(error) = &state.error
            {
                panel = panel.child(error.clone());
            }
            if let Some(kept) = state.partials_kept {
                panel = panel.child(if kept {
                    "Partial files kept; Download / resume can reuse them."
                } else {
                    "Partial files deleted; verified artifacts are kept."
                });
            }
        } else {
            panel = panel.child(if snapshot.pending {
                "No active helper · a recorded download is available."
            } else {
                "No active helper or recorded download."
            });
        }
        if let Some(cancel) = &snapshot.cancel_requested {
            panel = panel.child(if cancel == "delete" {
                "Cancellation requested · delete partial files"
            } else {
                "Cancellation requested · keep partial files"
            });
        }
        panel
            .child("Actions prepare commands for Send. Verified artifacts survive cancellation.")
            .into_any_element()
    }

    fn jobs_panel(&self, session: &Session, cx: &Context<Self>) -> gpui::AnyElement {
        let mut rows = div()
            .id("engine-jobs")
            .max_h(px(160.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .text_xs()
            .child("Last engine snapshot");
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        match self
            .snapshot
            .telemetry
            .get(&session.id)
            .and_then(|telemetry| telemetry.jobs.as_ref())
        {
            Some(snapshot) => {
                rows = rows.child(format!(
                    "{} running · {} tracked",
                    snapshot.running, snapshot.total
                ));
                for job in &snapshot.jobs {
                    let supports_actions = session.controls.as_ref().is_some_and(|controls| {
                        controls.commands.iter().any(|command| {
                            command.name == "/jobs" && command.args.contains("output")
                        })
                    });
                    let mut actions = div().flex().gap_1();
                    for (action, label) in [("output", "Output"), ("stop", "Stop")] {
                        let command = format!("/jobs {action} {}", job.id);
                        actions = actions.child(
                            Button::new(format!("job-{action}-{}", job.id), label)
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::Ghost)
                                .disabled(
                                    !supports_actions
                                        || self.remote_read_only(&session.id)
                                        || (action == "stop" && job.state != "running"),
                                )
                                .on_click(click(cx, move |view, window, cx| {
                                    view.prepare_command(&command, false, cx);
                                    view.composer_focus.focus(window, cx);
                                    cx.notify();
                                })),
                        );
                    }
                    rows = rows.child(
                        div()
                            .child(format!(
                                "#{} · PID {} · {:.1}s · {}{}",
                                job.id,
                                job.pid,
                                job.elapsed_secs,
                                job.state.replace('_', " "),
                                job.exit_code
                                    .map(|code| format!(" · exit {code}"))
                                    .unwrap_or_default()
                            ))
                            .child(
                                div()
                                    .text_color(colors.muted)
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(job.path.clone()),
                            )
                            .child(actions),
                    );
                }
                if (snapshot.jobs.len() as u64) < snapshot.total {
                    rows = rows.child("More jobs are tracked; use /jobs for the full table.");
                }
            }
            None => rows = rows.child(
                "Job snapshots are unavailable from this engine. Use /jobs to request its table.",
            ),
        }
        div()
            .child(rows)
            .child(
                Button::new("jobs-report", "Report")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .on_click(click(cx, |view, window, cx| {
                        view.prepare_command("/jobs", false, cx);
                        view.composer_focus.focus(window, cx);
                    })),
            )
            .into_any_element()
    }

    fn document_editor(
        &self,
        session_id: &str,
        editor: &crate::model::PendingApproval,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let key = document_key(session_id, editor);
        let Some(document) = self.documents.get(&key) else {
            return div().into_any_element();
        };
        let mut rows = div()
            .id("document-lines")
            .max_h(px(240.0))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1();
        // Bound input entities even for a document containing thousands of lines.
        let pages = document.pages();
        let page = document.page.min(pages - 1);
        let search_line = document
            .search
            .open
            .then(|| document.search_summary().2)
            .flatten();
        for (index, line) in document
            .lines
            .iter()
            .enumerate()
            .skip(page * crate::document::PAGE_LINES)
            .take(crate::document::PAGE_LINES)
        {
            let id = line.id.clone();
            let text_id = id.clone();
            let weak = cx.entity().downgrade();
            let text_key = key.clone();
            let end_key = key.clone();
            let mut input =
                crate::editor_input::Input::new(SharedString::from(format!("document-line-{id}")))
                    .size(crate::editor_input::InputSize::Sm)
                    .value(line.text.clone())
                    .on_text_change(move |text, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            if let Some(document) = view.documents.get_mut(&text_key) {
                                document.set(&text_id, text);
                            }
                            view.draft_dirty = true;
                            cx.notify();
                        });
                    })
                    .on_edit_end(document_edit_end(cx, end_key));
            if let Some(navigation) = &document.navigation
                && navigation.line_id == id
            {
                input = input.navigate(navigation.token, navigation.range.clone());
            }
            let add_key = key.clone();
            let add_id = id.clone();
            let remove_key = key.clone();
            let remove_id = id.clone();
            rows = rows.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .bg(if document.search.open && search_line == Some(index) {
                        colors.surface
                    } else {
                        gpui::transparent_black().into()
                    })
                    .child(
                        div()
                            .w(px(28.0))
                            .text_size(px(10.0))
                            .text_color(colors.muted)
                            .child((index + 1).to_string()),
                    )
                    .child(div().flex_1().min_w_0().child(input))
                    .child(
                        Button::new(SharedString::from(format!("insert-{id}")), "+")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .on_click(click(cx, move |view, _, cx| {
                                if let Some(document) = view.documents.get_mut(&add_key) {
                                    document.insert_after(&add_id);
                                }
                                view.draft_dirty = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("remove-{id}")), "−")
                            .size(ButtonSize::Xs)
                            .variant(ButtonVariant::Ghost)
                            .on_click(click(cx, move |view, _, cx| {
                                if let Some(document) = view.documents.get_mut(&remove_key) {
                                    document.remove(&remove_id);
                                }
                                view.draft_dirty = true;
                                cx.notify();
                            })),
                    ),
            );
        }
        let mut actions = div().flex().flex_wrap().items_center().gap_1();
        let find_key = key.clone();
        actions = actions.child(
            Button::new("editor-find", "Find")
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .on_click(click(cx, move |view, window, cx| {
                    if let Some(document) = view.documents.get_mut(&find_key) {
                        document.search.open = !document.search.open;
                        if document.search.open {
                            view.document_search_focus.focus(window, cx);
                        }
                    }
                    cx.notify();
                })),
        );
        for (label, undo, enabled) in [
            ("Undo", true, document.can_undo()),
            ("Redo", false, document.can_redo()),
        ] {
            let key = key.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("editor-{label}")), label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!enabled)
                    .on_click(click(cx, move |view, _, cx| {
                        if let Some(document) = view.documents.get_mut(&key) {
                            if undo {
                                document.undo();
                            } else {
                                document.redo();
                            }
                        }
                        view.draft_dirty = true;
                        cx.notify();
                    })),
            );
        }
        for (label, destination, disabled) in [
            ("‹", page.saturating_sub(1), page == 0),
            ("›", page + 1, page + 1 == pages),
        ] {
            let key = key.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("editor-page-{label}")), label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(disabled)
                    .on_click(click(cx, move |view, _, cx| {
                        if let Some(document) = view.documents.get_mut(&key) {
                            document.end_typing();
                            document.page = destination;
                        }
                        view.draft_dirty = true;
                        cx.notify();
                    })),
            );
        }
        actions = actions.child(
            div()
                .text_size(px(10.0))
                .text_color(colors.muted)
                .child(format!("{}/{}", page + 1, pages)),
        );
        let line_key = format!("{key}:line");
        let text_key = line_key.clone();
        let weak = cx.entity().downgrade();
        let destination = self.field(&line_key).parse::<usize>().ok();
        let valid = destination.is_some_and(|line| line > 0 && line <= document.lines.len());
        let go_key = key.clone();
        actions = actions
            .child(
                div().w(px(70.0)).child(
                    Input::new(SharedString::from(line_key.clone()))
                        .size(gpui_ui_kit::InputSize::Sm)
                        .placeholder("Line")
                        .aria_label(format!("Go to line, 1 through {}", document.lines.len()))
                        .value(self.field(&line_key))
                        .on_text_change(move |text, _, cx| {
                            let _ = weak.update(cx, |view, cx| {
                                view.fields.insert(text_key.clone(), text.into());
                                cx.notify();
                            });
                        }),
                ),
            )
            .child(
                Button::new("editor-go-line", "Go")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(!valid)
                    .on_click(click(cx, move |view, _, cx| {
                        if let Some(document) = view.documents.get_mut(&go_key)
                            && let Some(line) = destination
                        {
                            document.go_to_line(line);
                        }
                        view.draft_dirty = true;
                        cx.notify();
                    })),
            );
        for (label, save) in [("Save", true), ("Cancel", false)] {
            let id = session_id.to_owned();
            let approval_id = editor.id.clone();
            let requirement = editor.requirement.clone();
            let key = key.clone();
            actions = actions.child(
                Button::new(SharedString::from(format!("document-{label}")), label)
                    .size(ButtonSize::Xs)
                    .disabled(self.remote_read_only(session_id))
                    .variant(if save {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .on_click(click(cx, move |view, _, cx| {
                        let text = if save {
                            view.documents
                                .get(&key)
                                .map(crate::document::Document::text)
                        } else {
                            None
                        };
                        view.command(Command::EditDocument {
                            session_id: id.clone(),
                            approval_id: approval_id.clone(),
                            requirement: requirement.clone(),
                            text,
                        });
                        cx.notify();
                    })),
            );
        }
        let history_key = key.clone();
        div()
            .id(SharedString::from(format!("document-editor-{key}")))
            .capture_key_down(
                cx.listener(move |view, event: &gpui::KeyDownEvent, window, cx| {
                    let modifiers = event.keystroke.modifiers;
                    #[cfg(target_os = "macos")]
                    let find_modifier = modifiers.platform;
                    #[cfg(not(target_os = "macos"))]
                    let find_modifier = modifiers.platform || modifiers.control;
                    if (event.keystroke.key == "f3"
                        || (event.keystroke.key == "g" && find_modifier && !modifiers.alt))
                        && let Some(document) = view.documents.get_mut(&history_key)
                        && document.search.open
                    {
                        document.find(modifiers.shift);
                        view.draft_dirty = true;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if event.keystroke.key == "enter"
                        && view.document_search_focus.is_focused(window)
                        && !(modifiers.platform || modifiers.control || modifiers.alt)
                    {
                        if let Some(document) = view.documents.get_mut(&history_key) {
                            document.find(modifiers.shift);
                        }
                        view.draft_dirty = true;
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if event.keystroke.key == "f" && find_modifier && !modifiers.alt {
                        if let Some(document) = view.documents.get_mut(&history_key) {
                            document.search.open = true;
                        }
                        view.document_search_focus.focus(window, cx);
                        cx.stop_propagation();
                        cx.notify();
                        return;
                    }
                    if view.document_search_focus.is_focused(window) {
                        return;
                    }
                    if !(modifiers.platform || modifiers.control) || modifiers.alt {
                        return;
                    }
                    let redo = match event.keystroke.key.as_str() {
                        "z" => modifiers.shift,
                        "y" if modifiers.control => true,
                        _ => return,
                    };
                    if let Some(document) = view.documents.get_mut(&history_key) {
                        if redo {
                            document.redo();
                        } else {
                            document.undo();
                        }
                    }
                    view.draft_dirty = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .border_1()
            .border_color(colors.muted)
            .rounded(px(5.0))
            .child(div().text_xs().child(editor.tool.clone()))
            .child(rows)
            .child(self.document_search(&key, document, cx))
            .child(actions)
            .into_any_element()
    }

    fn document_search(
        &self,
        key: &str,
        document: &crate::document::Document,
        cx: &Context<Self>,
    ) -> gpui::AnyElement {
        if !document.search.open {
            return div().into_any_element();
        }
        let (current, total, _) = document.search_summary();
        let search_key = key.to_owned();
        let weak = cx.entity().downgrade();
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_1()
            .child(
                div().w(px(180.0)).child(
                    Input::new(SharedString::from(format!("document-search-{key}")))
                        .focus_handle(self.document_search_focus.clone())
                        .size(gpui_ui_kit::InputSize::Sm)
                        .placeholder("Find (case sensitive)")
                        .value(document.search.query.clone())
                        .on_text_change(move |text, _, cx| {
                            let _ = weak.update(cx, |view, cx| {
                                if let Some(document) = view.documents.get_mut(&search_key) {
                                    document.search_for(text);
                                }
                                view.draft_dirty = true;
                                cx.notify();
                            });
                        }),
                ),
            )
            .child(
                div()
                    .text_xs()
                    .child(format!("{}/{}", current.unwrap_or(0), total)),
            );
        for (id, label, backwards) in [("find-prev", "‹", true), ("find-next", "›", false)] {
            let key = key.to_owned();
            row = row.child(
                Button::new(id, label)
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Ghost)
                    .disabled(total == 0)
                    .on_click(click(cx, move |view, _, cx| {
                        if let Some(document) = view.documents.get_mut(&key) {
                            document.find(backwards);
                        }
                        view.draft_dirty = true;
                        cx.notify();
                    })),
            );
        }
        let replacement_key = key.to_owned();
        let weak = cx.entity().downgrade();
        row = row.child(
            div().w(px(160.0)).child(
                Input::new(SharedString::from(format!("document-replace-{key}")))
                    .size(gpui_ui_kit::InputSize::Sm)
                    .placeholder("Replace with")
                    .value(document.search.replacement.clone())
                    .on_text_change(move |text, _, cx| {
                        let _ = weak.update(cx, |view, cx| {
                            if let Some(document) = view.documents.get_mut(&replacement_key) {
                                document.search.replacement = text.into();
                            }
                            cx.notify();
                        });
                    }),
            ),
        );
        let replacement_key = key.to_owned();
        row = row.child(
            Button::new("document-replace-match", "Replace")
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .disabled(current.is_none() || document.search.query == document.search.replacement)
                .on_click(click(cx, move |view, _, cx| {
                    if let Some(document) = view.documents.get_mut(&replacement_key)
                        && document.replace_match()
                    {
                        view.draft_dirty = true;
                    }
                    cx.notify();
                })),
        );
        row.into_any_element()
    }

    fn details_panel(&self, session: &Session, cx: &mut Context<Self>) -> gpui::AnyElement {
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let id = session.id.clone();
        let archive_id = id.clone();
        let archived = session.archived;
        let mut panel = div()
            .id("session-details")
            .p_4()
            .h_full()
            .overflow_y_scroll()
            .child(
                layout! { B.ghost("Back to conversation") #close_details }.on_click(click(
                    cx,
                    |view, _, cx| {
                        view.details = false;
                        cx.notify();
                    },
                )),
            )
            .child(layout! { V[g4] { Hd "Session details" [level="3"] }})
            .child(self.input("rename-session", "Agent name", cx))
            .child(
                layout! { B("Rename") #rename_agent }.on_click(click(cx, move |view, _, cx| {
                    view.command(Command::RenameSession {
                        session_id: id.clone(),
                        name: view.field("rename-session"),
                    });
                    cx.notify();
                })),
            )
            .child(div().text_sm().child(format!(
                "{} · {}",
                backend_label(&session.backend),
                state_label(session)
            )))
            .child(div().text_sm().child(format!(
                "Conversation: {}",
                session.agent_session.as_deref().unwrap_or("Not started")
            )))
            .child(
                Button::new(
                    "archive-agent",
                    if archived {
                        "Restore agent"
                    } else {
                        "Archive agent"
                    },
                )
                .disabled(session.state.busy())
                .on_click(click(cx, move |view, _, cx| {
                    view.command(Command::ArchiveSession {
                        session_id: archive_id.clone(),
                        archived: !archived,
                    });
                    cx.notify();
                })),
            );
        if let Some(telemetry) = self.snapshot.telemetry.get(&session.id) {
            panel = panel.child(div().text_sm().child(telemetry.summary()));
            if let Some(status) = &telemetry.status {
                panel = panel.child(div().text_sm().child(format!(
                    "{} generated tokens · {:.1}s elapsed",
                    status.generated, status.elapsed_secs
                )));
            }
        }
        if let Some(controls) = &session.controls
            && !controls.features.is_empty()
        {
            let expanded = self
                .fields
                .get("engine-capabilities-expanded")
                .is_some_and(|value| value == "true");
            let available = controls
                .features
                .iter()
                .filter(|feature| feature.available)
                .count();
            panel = panel.child(
                Button::new(
                    "engine-capabilities-toggle",
                    format!(
                        "{} Engine capabilities · {available}/{} available",
                        if expanded { "▾" } else { "▸" },
                        controls.features.len()
                    ),
                )
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .on_click(click(cx, move |view, _, cx| {
                    view.fields.insert(
                        "engine-capabilities-expanded".into(),
                        (!expanded).to_string(),
                    );
                    cx.notify();
                })),
            );
            let mut capabilities = div()
                .id("engine-capabilities-list")
                .max_h(px(160.0))
                .overflow_y_scroll()
                .flex()
                .flex_col();
            if expanded {
                for feature in &controls.features {
                    capabilities = capabilities.child(
                        div()
                            .mt_1()
                            .text_xs()
                            .child(format!(
                                "{} · {}",
                                feature.label,
                                if feature.available {
                                    "Available"
                                } else {
                                    "Unavailable"
                                }
                            ))
                            .child(div().text_color(colors.muted).child(feature.detail.clone())),
                    );
                }
                panel = panel.child(capabilities);
            }
        }
        let peers: Vec<_> = self
            .snapshot
            .sessions
            .iter()
            .filter(|s| s.workspace_id == session.workspace_id && !s.archived)
            .collect();
        if let Some(index) = peers.iter().position(|s| s.id == session.id) {
            for (label, adjacent) in [
                ("Move up", index.checked_sub(1)),
                ("Move down", (index + 1 < peers.len()).then_some(index + 1)),
            ] {
                if let Some(adjacent) = adjacent {
                    let current = session.id.clone();
                    let other_position = peers[adjacent].position;
                    panel = panel.child(Button::new(SharedString::from(label), label).on_click(
                        click(cx, move |view, _, cx| {
                            view.command(Command::ReorderSession {
                                session_id: current.clone(),
                                position: other_position,
                            });
                            cx.notify();
                        }),
                    ));
                }
            }
        }
        for workspace in self
            .snapshot
            .workspaces
            .iter()
            .filter(|workspace| workspace.id != session.workspace_id)
        {
            let id = session.id.clone();
            let destination = workspace.id.clone();
            panel = panel.child(
                Button::new(
                    SharedString::from(format!("move-to-{destination}")),
                    format!("Move to {}", workspace.name),
                )
                .size(ButtonSize::Xs)
                .variant(ButtonVariant::Ghost)
                .disabled(session.state.busy())
                .on_click(click(cx, move |view, _, cx| {
                    view.command(Command::MoveSession {
                        session_id: id.clone(),
                        workspace_id: destination.clone(),
                    });
                    cx.notify();
                })),
            );
        }
        if let Some(workspace) = self
            .snapshot
            .workspaces
            .iter()
            .find(|w| w.id == session.workspace_id)
        {
            let workspace_id = workspace.id.clone();
            panel = panel
                .child(div().text_sm().child(workspace.directory.clone()))
                .child(self.input("rename-workspace", "Workspace name", cx))
                .child(
                    layout! { B("Rename workspace") #rename_workspace }.on_click(click(
                        cx,
                        move |view, _, cx| {
                            view.command(Command::RenameWorkspace {
                                workspace_id: workspace_id.clone(),
                                name: view.field("rename-workspace"),
                            });
                            cx.notify();
                        },
                    )),
                );
        }
        panel.into_any_element()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut visible_inputs = HashSet::new();
        if !self.appearance
            && let Some(session) = self.selected.as_ref().and_then(|id| {
                self.snapshot
                    .sessions
                    .iter()
                    .find(|session| &session.id == id)
            })
        {
            for editor in session
                .approvals
                .iter()
                .filter(|editor| editor.requirement["kind"] == "plank_editor")
            {
                if let Some(document) = self.documents.get(&document_key(&session.id, editor)) {
                    for line in document
                        .lines
                        .iter()
                        .skip(document.page.min(document.pages() - 1) * crate::document::PAGE_LINES)
                        .take(crate::document::PAGE_LINES)
                    {
                        visible_inputs.insert(gpui::ElementId::from(SharedString::from(format!(
                            "document-line-{}",
                            line.id
                        ))));
                    }
                }
            }
        }
        crate::editor_input::cleanup_stale_input_states(&visible_inputs);
        if self.focus_commands {
            self.command_focus.focus(window, cx);
            self.focus_commands = false;
        }
        let colors = WorkspaceTheme::from(cx.theme().as_ref());
        let compact = window.viewport_size().width < px(700.0);
        let selected = self
            .selected
            .as_ref()
            .and_then(|id| self.snapshot.sessions.iter().find(|s| &s.id == id));
        let content = if self.appearance {
            div()
                .id("appearance-scroll")
                .p_6()
                .overflow_y_scroll()
                .h_full()
                .child(self.appearance_panel(cx))
                .into_any_element()
        } else if let Some(session) = selected {
            self.conversation(session, &colors, cx)
        } else {
            div().p_6().child(layout! { V[g6] { Hd "Your agents, one workspace" [level="1"], Tx "Create a workspace, then add local, remote, or demo sessions." [muted] }}).into_any_element()
        };
        let mut body = div().flex().flex_1().min_h_0();
        // Requests have been copied into this frame's input props. Keeping them
        // in the document would replay a focus change when revisiting a page.
        for document in self.documents.values_mut() {
            document.navigation = None;
        }
        if !self.appearance_only && (!compact || self.mobile_sidebar) {
            body = body.child(
                div()
                    .w(px(if compact { 210.0 } else { 270.0 }))
                    .flex_shrink_0()
                    .child(self.sidebar(&colors, cx)),
            );
        }
        body = body.child(div().flex_1().min_w_0().h_full().child(content));
        if self.details
            && !self.show_new
            && !self.appearance
            && let Some(session) = selected
        {
            let details = self.details_panel(session, cx);
            if compact {
                body = div().flex_1().min_h_0().child(details);
            } else {
                body = body.child(div().w(px(300.0)).flex_shrink_0().child(details));
            }
        }
        let mut root = div()
            .capture_key_down(
                cx.listener(|view, event, window, cx| view.control_key(event, window, cx)),
            )
            .w(window.viewport_size().width)
            .h(window.viewport_size().height)
            .overflow_hidden()
            .font_family(ui_font())
            .text_size(px(14.0))
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.text);
        #[cfg(target_os = "macos")]
        {
            let utilization = self.meters;
            let meter = |icon: &'static str, value: Option<u8>| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .w(px(64.0))
                    .flex_shrink_0()
                    .child(
                        gpui::svg()
                            .path(icon)
                            .size(px(16.0))
                            .text_color(colors.muted),
                    )
                    .child(
                        div()
                            .w(px(38.0))
                            .flex_shrink_0()
                            .text_right()
                            .font_family("Menlo")
                            .child(
                                value.map_or_else(|| "--%".to_owned(), |value| format!("{value}%")),
                            ),
                    )
            };
            root = root.child(
                div()
                    .id("macos-titlebar")
                    .h(px(36.0))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .pl(px(88.0))
                    .pr_3()
                    .gap_3()
                    .text_xs()
                    .bg(colors.surface)
                    .window_control_area(gpui::WindowControlArea::Drag)
                    .on_mouse_down(gpui::MouseButton::Left, |event, window, _| {
                        if event.click_count == 2 {
                            window.titlebar_double_click();
                        }
                    })
                    .child(div().flex_1().min_w_0().child(if self.appearance {
                        "Preferences".to_owned()
                    } else {
                        self.snapshot
                            .sessions
                            .iter()
                            .find(|session| Some(&session.id) == self.selected.as_ref())
                            .map(|session| session.name.clone())
                            .unwrap_or_default()
                    }))
                    .child(meter("icons/cpu.svg", utilization.cpu))
                    .child(meter("icons/gpu.svg", utilization.gpu)),
            );
        }
        if compact && !self.appearance_only {
            root = root.child(
                layout! { B.ghost("Workspaces") #mobile_workspaces }.on_click(click(
                    cx,
                    |view, _, cx| {
                        view.mobile_sidebar = !view.mobile_sidebar;
                        cx.notify();
                    },
                )),
            );
        }
        if let Some(error) = &self.error {
            root = root.child(
                div()
                    .p_3()
                    .bg(colors.surface)
                    .text_color(colors.accent)
                    .child(error.clone()),
            );
        }
        root = root.child(body).child(
            div()
                .px_4()
                .py_2()
                .text_xs()
                .text_color(colors.muted)
                .child(if self.appearance {
                    "Appearance preferences apply to other open windows"
                } else if self.connected {
                    "Connected · execution continues when this window closes"
                } else {
                    "Connecting to Plank daemon…"
                }),
        );
        if self.show_new {
            root = root.relative().child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.35))
                    .p_4()
                    .child(
                        div()
                            .id("add-agent-popup")
                            .w(px(520.0))
                            .max_w_full()
                            .max_h_full()
                            .overflow_y_scroll()
                            .bg(colors.background)
                            .border_1()
                            .border_color(colors.muted)
                            .rounded(px(10.0))
                            .p_4()
                            .child(
                                div().flex().justify_end().child(
                                    Button::new("close-add-agent", "Cancel")
                                        .variant(ButtonVariant::Ghost)
                                        .on_click(click(cx, |view, _, cx| {
                                            view.show_new = false;
                                            cx.notify();
                                        })),
                                ),
                            )
                            .child(self.new_panel(cx)),
                    ),
            );
        }
        root
    }
}

fn state_label(session: &Session) -> &'static str {
    match session.state {
        crate::model::RunState::Idle => "○",
        crate::model::RunState::Starting => "◌ starting",
        crate::model::RunState::Running => "● running",
        crate::model::RunState::Disconnected => "◇ disconnected",
        crate::model::RunState::Interrupted => "■ interrupted",
        crate::model::RunState::Failed => "! failed",
    }
}

fn document_key(session_id: &str, editor: &crate::model::PendingApproval) -> String {
    format!(
        "{session_id}:{}:{}",
        editor.id,
        editor.requirement["revision"].as_str().unwrap_or("")
    )
}

fn export_key(session: &str, generation: u64, id: &str) -> String {
    format!("{session}:{generation}:{id}")
}

#[cfg(target_family = "wasm")]
fn snapshot_has_export(snapshot: &Snapshot, key: &str) -> bool {
    snapshot.sessions.iter().any(|session| {
        snapshot
            .telemetry
            .get(&session.id)
            .is_some_and(|telemetry| {
                [&telemetry.export, &telemetry.insights_export]
                    .into_iter()
                    .flatten()
                    .any(|export| {
                        export_key(&session.id, session.generation, &export.id) == key
                            && (export.purpose != crate::model::ExportPurpose::Insights
                                || telemetry
                                    .metrics
                                    .get("insights")
                                    .is_none_or(|metrics| export.matches_insights(metrics)))
                    })
            })
    })
}

fn document_edit_end(
    cx: &Context<WorkspaceView>,
    key: String,
) -> impl Fn(Option<String>, &mut Window, &mut App) + 'static {
    let weak = cx.entity().downgrade();
    move |_, _, cx| {
        let _ = weak.update(cx, |view, _| {
            if let Some(document) = view.documents.get_mut(&key) {
                document.end_typing();
            }
        });
    }
}

fn click<F>(
    cx: &Context<WorkspaceView>,
    handler: F,
) -> impl Fn(&mut Window, &mut App) + 'static + use<F>
where
    F: Fn(&mut WorkspaceView, &mut Window, &mut Context<WorkspaceView>) + 'static,
{
    let weak = cx.entity().downgrade();
    move |window, cx| {
        let _ = weak.update(cx, |view, cx| handler(view, window, cx));
    }
}

fn backend_label(backend: &Backend) -> &'static str {
    match backend {
        Backend::Demo => "Demo",
        Backend::Local { .. } => "Local Plank",
        Backend::Remote { .. } => "Remote Plank",
        Backend::Agent { provider, .. } => provider.label(),
    }
}

fn design(index: usize) -> DesignSystem {
    match index % 7 {
        0 => DesignSystem::neutral(),
        1 => DesignSystem::apple_hig(),
        2 => DesignSystem::material3(),
        3 => DesignSystem::fluent(),
        4 => DesignSystem::adwaita(),
        5 => DesignSystem::breeze(),
        _ => DesignSystem::carbon(),
    }
}

fn design_name(index: usize) -> &'static str {
    [
        "Neutral",
        "Apple",
        "Material 3",
        "Fluent",
        "Adwaita",
        "Breeze",
        "Carbon",
    ][index % 7]
}

fn load_preferences(directory: &std::path::Path) -> Preferences {
    #[cfg(not(target_family = "wasm"))]
    let bytes = std::fs::read(directory.join("preferences.json")).ok();
    #[cfg(target_family = "wasm")]
    let bytes = {
        let _ = directory;
        browser_preferences().map(String::into_bytes)
    };
    bytes
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

#[cfg(target_family = "wasm")]
fn browser_preferences() -> Option<String> {
    web_sys::window()
        .and_then(|window| window.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item("nplank.preferences").ok().flatten())
}

fn bridge(
    client: Client,
    selected: Option<String>,
) -> (mpsc::Sender<Operation>, mpsc::Receiver<Update>) {
    let (tx, input) = mpsc::channel();
    let (output, rx) = mpsc::sync_channel(4);
    #[cfg(not(target_family = "wasm"))]
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = output.send(Update::Error(error.to_string()));
                return;
            }
        };
        runtime.block_on(run_bridge(client, selected, input, output));
    });
    #[cfg(target_family = "wasm")]
    wasm_bindgen_futures::spawn_local(run_bridge(client, selected, input, output));
    (tx, rx)
}

async fn run_bridge(
    client: Client,
    mut selected: Option<String>,
    input: mpsc::Receiver<Operation>,
    output: mpsc::SyncSender<Update>,
) {
    let mut before = 0;
    loop {
        loop {
            match input.try_recv() {
                Ok(Operation::Select(id, page)) => {
                    selected = id;
                    before = page;
                }
                Ok(Operation::Command(command)) => {
                    let select = matches!(command, Command::CreateSession { .. });
                    let workspace = matches!(command, Command::CreateWorkspace { .. });
                    let prompt = match &command {
                        Command::Prompt {
                            session_id, text, ..
                        } => Some((session_id.clone(), text.clone())),
                        _ => None,
                    };
                    let update = match client.execute(&command).await {
                        Ok(receipt) if select => Update::Selected(receipt.id),
                        Ok(receipt) if workspace => Update::WorkspaceCreated(receipt.id),
                        Ok(_) => Update::Accepted(prompt),
                        Err(error) => Update::Error(error.to_string()),
                    };
                    if !send_update(&output, update).await {
                        return;
                    }
                }
                Ok(Operation::Download(request)) => {
                    let client = client.clone();
                    let output = output.clone();
                    #[cfg(not(target_family = "wasm"))]
                    tokio::spawn(async move {
                        let result = client
                            .save_export(
                                &request.session,
                                request.generation,
                                &request.export,
                                &request.path,
                            )
                            .await
                            .map(|()| format!("Saved to {}", request.path.display()))
                            .map_err(|error| error.to_string());
                        send_update(&output, Update::ExportFinished(request.key, result)).await;
                    });
                    #[cfg(target_family = "wasm")]
                    wasm_bindgen_futures::spawn_local(async move {
                        let update = match client
                            .browser_export(&request.session, request.generation, &request.export)
                            .await
                        {
                            Ok(url) => Update::ExportReady(
                                request.key,
                                url,
                                request.export.suggested_name(),
                            ),
                            Err(error) => {
                                Update::ExportFinished(request.key, Err(error.to_string()))
                            }
                        };
                        send_update(&output, update).await;
                    });
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }
        let update = match client.snapshot(selected.as_deref(), before).await {
            Ok(snapshot) => Update::Snapshot(selected.clone(), snapshot),
            Err(error) => Update::Error(format!("Daemon disconnected: {error}")),
        };
        if !send_update(&output, update).await {
            return;
        }
        bridge_sleep(300).await;
    }
}

// Never block the browser's main thread when its bounded update queue is full.
async fn send_update(output: &mpsc::SyncSender<Update>, mut update: Update) -> bool {
    loop {
        match output.try_send(update) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Disconnected(update)) => {
                #[cfg(target_family = "wasm")]
                if let Update::ExportReady(_, url, _) = update {
                    let _ = web_sys::Url::revoke_object_url(&url);
                }
                #[cfg(not(target_family = "wasm"))]
                let _ = update;
                return false;
            }
            Err(mpsc::TrySendError::Full(pending)) => update = pending,
        }
        bridge_sleep(30).await;
    }
}

async fn bridge_sleep(milliseconds: i32) {
    #[cfg(not(target_family = "wasm"))]
    tokio::time::sleep(Duration::from_millis(milliseconds as u64)).await;
    #[cfg(target_family = "wasm")]
    crate::web::sleep(milliseconds).await;
}

fn provider_choice(choice: usize) -> AgentProvider {
    match choice {
        3 => AgentProvider::MuseCode,
        4 => AgentProvider::Codex,
        _ => AgentProvider::ClaudeCode,
    }
}

fn ui_font() -> &'static str {
    if cfg!(target_family = "wasm") {
        "IBM Plex Sans"
    } else if cfg!(target_os = "macos") {
        "Helvetica"
    } else if cfg!(target_os = "windows") {
        "Segoe UI"
    } else {
        "DejaVu Sans"
    }
}
