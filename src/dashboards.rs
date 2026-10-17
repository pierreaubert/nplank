//! Grafana-style dashboards stored as JSON in the workspace.
//!
//! Layout: `<workspace>/.nplank/dashboards/<slug>.json`, one dashboard
//! per file, so dashboards are versionable in git and editable by hand.

// Rust guideline compliant 2026-02-21
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum panels per dashboard: keeps files and panels readable.
const MAX_PANELS: usize = 24;

/// How one panel draws its series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelKind {
    /// Multi-series line chart (d3rs monotone-x interpolation).
    Line,
    /// Filled area chart.
    Area,
    /// Latest value per series as horizontal bars.
    Bar,
    /// Latest value as a large number, with change over the range.
    Stat,
}

impl PanelKind {
    /// All kinds, for the editor's cycle order.
    pub const ALL: [Self; 4] = [Self::Line, Self::Area, Self::Bar, Self::Stat];

    /// Short display label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Line => "Line",
            Self::Area => "Area",
            Self::Bar => "Bar",
            Self::Stat => "Stat",
        }
    }

    /// Next kind in `ALL` order (editor cycle button).
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

/// One dashboard panel: a titled PromQL query and its chart kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Panel {
    /// Panel heading.
    pub title: String,
    /// Chart kind.
    pub kind: PanelKind,
    /// PromQL expression evaluated with `query_range`.
    pub query: String,
    /// Optional unit suffix for stat/bar values (`%`, `ms`, …).
    #[serde(default)]
    pub unit: String,
}

/// One named dashboard: an ordered panel grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Dashboard {
    /// Display name; also derives the file slug.
    pub name: String,
    /// Panels in display order.
    pub panels: Vec<Panel>,
}

impl Dashboard {
    /// A starter dashboard seeded with universally available queries.
    pub fn starter() -> Self {
        Self {
            name: "Overview".into(),
            panels: vec![
                Panel {
                    title: "Targets up".into(),
                    kind: PanelKind::Line,
                    query: "up".into(),
                    unit: String::new(),
                },
                Panel {
                    title: "Scrape duration".into(),
                    kind: PanelKind::Stat,
                    query: "scrape_duration_seconds".into(),
                    unit: "s".into(),
                },
            ],
        }
    }

    /// Validation errors, empty when the dashboard may be saved.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();
        if slug(&self.name).is_empty() {
            errors.push("Dashboard name must contain a letter or digit".into());
        }
        if self.panels.len() > MAX_PANELS {
            errors.push(format!("At most {MAX_PANELS} panels per dashboard"));
        }
        for (index, panel) in self.panels.iter().enumerate() {
            if panel.title.trim().is_empty() {
                errors.push(format!("Panel {} needs a title", index + 1));
            }
            if panel.query.trim().is_empty() {
                errors.push(format!("Panel {} needs a PromQL query", index + 1));
            }
        }
        errors
    }
}

/// Filesystem slug for a dashboard name (`My Dash!` → `my-dash`).
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Dashboard directory for a workspace root.
pub fn dashboards_dir(workspace: &std::path::Path) -> PathBuf {
    workspace.join(".nplank").join("dashboards")
}

/// Lists dashboards in `workspace`, sorted by name; unreadable or invalid
/// files are skipped (they stay on disk for hand-fixing).
pub fn list(workspace: &std::path::Path) -> Vec<Dashboard> {
    let dir = dashboards_dir(workspace);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    let mut dashboards: Vec<Dashboard> = paths
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| serde_json::from_str::<Dashboard>(&text).ok())
        .filter(|dashboard| dashboard.validate().is_empty())
        .collect();
    dashboards.sort_by(|a, b| a.name.cmp(&b.name));
    dashboards.dedup_by(|a, b| slug(&a.name) == slug(&b.name));
    dashboards
}

/// Saves `dashboard` to its slug file, creating the directory.
///
/// # Errors
/// Returns validation errors or the I/O error that stopped the write.
pub fn save(workspace: &std::path::Path, dashboard: &Dashboard) -> Result<PathBuf, String> {
    let errors = dashboard.validate();
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    let dir = dashboards_dir(workspace);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
    let path = dir.join(format!("{}.json", slug(&dashboard.name)));
    let text = serde_json::to_string_pretty(dashboard)
        .map_err(|error| format!("Could not encode dashboard: {error}"))?;
    std::fs::write(&path, format!("{text}\n"))
        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
    Ok(path)
}

/// Deletes the dashboard file for `name`; missing files are not an error.
///
/// # Errors
/// Returns the I/O error that stopped the delete.
pub fn delete(workspace: &std::path::Path, name: &str) -> Result<(), String> {
    let path = dashboards_dir(workspace).join(format!("{}.json", slug(name)));
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("Could not delete {}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_are_filesystem_safe() {
        assert_eq!(slug("My Dash!"), "my-dash");
        assert_eq!(slug("  --  "), "");
        assert_eq!(slug("CPU / mem"), "cpu-mem");
        assert_eq!(PanelKind::Line.next(), PanelKind::Area);
        assert_eq!(PanelKind::Stat.next(), PanelKind::Line);
    }

    #[test]
    fn save_list_delete_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut dashboard = Dashboard::starter();
        assert!(list(dir.path()).is_empty());
        save(dir.path(), &dashboard).expect("save");
        dashboard.panels.push(Panel {
            title: "Jobs".into(),
            kind: PanelKind::Bar,
            query: "count by (job) (up)".into(),
            unit: String::new(),
        });
        save(dir.path(), &dashboard).expect("save again");
        let listed = list(dir.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].panels.len(), 3);
        // Invalid files are skipped, not fatal.
        std::fs::write(dashboards_dir(dir.path()).join("broken.json"), "{nope").expect("write");
        assert_eq!(list(dir.path()).len(), 1);
        delete(dir.path(), "Overview").expect("delete");
        assert!(list(dir.path()).is_empty());
        delete(dir.path(), "Overview").expect("delete missing is fine");
    }

    #[test]
    fn validation_rejects_empty_fields() {
        let dashboard = Dashboard {
            name: "!!!".into(),
            panels: vec![Panel {
                title: String::new(),
                kind: PanelKind::Line,
                query: String::new(),
                unit: String::new(),
            }],
        };
        assert_eq!(dashboard.validate().len(), 3);
        assert!(save(std::path::Path::new("/nonexistent"), &dashboard).is_err());
    }
}
