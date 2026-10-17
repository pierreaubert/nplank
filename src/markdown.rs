//! Read-only Markdown viewer support: discover `.md` files in a
//! workspace directory for display with the GPUI `Markdown` component.

use std::path::PathBuf;

/// Maximum directory depth below the workspace root.
const MAX_DEPTH: usize = 3;
/// Directory names never descended into.
const SKIPPED: &[&str] = &["target", "node_modules", ".git"];

fn walk(dir: &std::path::Path, depth: usize, out: &mut Vec<PathBuf>, limit: usize) {
    if depth > MAX_DEPTH || out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if out.len() >= limit {
            return;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if !name.starts_with('.') && !SKIPPED.contains(&name.as_str()) {
                walk(&path, depth + 1, out, limit);
            }
        } else if name.to_ascii_lowercase().ends_with(".md") {
            out.push(path);
        }
    }
}

/// Markdown files under `root`, sorted, capped at `limit`.
pub fn find_markdown_files(root: &std::path::Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, 0, &mut out, limit);
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_md_skipping_hidden_and_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("README.md"), "# hi").expect("write");
        std::fs::create_dir(dir.path().join("docs")).expect("mkdir");
        std::fs::write(dir.path().join("docs/guide.markdown"), "x").expect("write");
        std::fs::write(dir.path().join("docs/notes.md"), "x").expect("write");
        std::fs::create_dir(dir.path().join("target")).expect("mkdir");
        std::fs::write(dir.path().join("target/gen.md"), "x").expect("write");
        std::fs::create_dir(dir.path().join(".hidden")).expect("mkdir");
        std::fs::write(dir.path().join(".hidden/secret.md"), "x").expect("write");
        let found = find_markdown_files(dir.path(), 50);
        let names: Vec<String> = found
            .iter()
            .map(|path| {
                path.strip_prefix(dir.path())
                    .expect("prefix")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["README.md", "docs/notes.md"]);
    }
}
