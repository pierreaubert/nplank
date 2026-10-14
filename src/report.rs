//! Bounded Insights report blocks for the shared native and browser view.

// Rust guideline compliant 2026-02-21

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Heading(String),
    Text(String),
    Code(String),
    Table(Vec<Vec<String>>),
}

pub(crate) fn parse(source: &str) -> Vec<Block> {
    let mut lines = source.lines().peekable();
    let mut blocks = Vec::new();
    while let Some(line) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(line) = line.strip_prefix("    ") {
            let mut code = line.to_owned();
            while let Some(line) = lines.next_if(|line| line.starts_with("    ")) {
                code.push('\n');
                code.push_str(line.strip_prefix("    ").expect("indented line selected"));
            }
            blocks.push(Block::Code(code));
        } else if line.starts_with('|') {
            let mut table = vec![cells(line)];
            while let Some(row) = lines.next_if(|line| line.starts_with('|')) {
                let row = cells(row);
                if row.iter().all(|cell| {
                    !cell.is_empty() && cell.chars().all(|c| matches!(c, '-' | ':' | ' '))
                }) {
                    continue;
                }
                table.push(row);
            }
            blocks.push(Block::Table(table));
        } else if let Some(heading) = line
            .strip_prefix('#')
            .map(|line| line.trim_start_matches('#').trim())
        {
            blocks.push(Block::Heading(unescape(heading)));
        } else if let Some(heading) = line
            .strip_prefix("**")
            .and_then(|line| line.strip_suffix("**"))
        {
            blocks.push(Block::Heading(unescape(heading)));
        } else {
            blocks.push(Block::Text(unescape(line)));
        }
    }
    blocks
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(chars.next().unwrap_or(c));
        } else {
            out.push(c);
        }
    }
    out
}

fn cells(line: &str) -> Vec<String> {
    let mut row = Vec::new();
    let mut cell = String::new();
    let mut chars = line.trim_matches('|').chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => cell.push(chars.next().unwrap_or(c)),
            '|' => {
                row.push(cell.trim().to_owned());
                cell.clear();
            }
            _ => cell.push(c),
        }
    }
    row.push(cell.trim().to_owned());
    row
}

#[cfg(test)]
mod tests {
    use super::{Block, parse};

    #[test]
    fn reports_render_aligned_tables_escaped_labels_and_multiline_snippets() {
        let blocks = parse(
            "# Insights\n\n| Tool | Count |\n| --- | ---: |\n| a\\|b | 7 |\n\n**Prompt**\n\n    first line\n    second line\n\nCheck \\<results\\>.\n",
        );
        assert_eq!(
            blocks,
            vec![
                Block::Heading("Insights".into()),
                Block::Table(vec![
                    vec!["Tool".into(), "Count".into()],
                    vec!["a|b".into(), "7".into()]
                ]),
                Block::Heading("Prompt".into()),
                Block::Code("first line\nsecond line".into()),
                Block::Text("Check <results>.".into()),
            ]
        );
    }
}
