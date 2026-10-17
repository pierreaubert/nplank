# CI board, Markdown docs, and tool launchers

Three workspace-scoped panels, reachable from the sidebar buttons, the
View menu (`CI Runs`, `Markdown Docs`), or `Cmd+Shift+I` / `Cmd+Shift+D`.
Fleet, CI, Docs, terminal, and conversation are mutually exclusive in the
content area, so opening one never appears to do nothing.

## CI runs board (`src/ci.rs`, `ci_panel` in `src/ui.rs`)

- Scope: the selected session's workspace (else the first workspace with
  a directory).
- Forge detection from `git remote get-url origin`: `github.com` →
  GitHub, anything else → Gitea.
- Fetch runs off the UI thread through the CLIs the user is already
  authenticated with — no tokens stored in nplank:
  - GitHub: `gh run list --limit 25 --json databaseId,name,status,conclusion,createdAt,updatedAt,url,event,headBranch`
  - Gitea: `tea actions runs list --limit 25 -o json` (parser tolerates
    both tea and Gitea API field names; an empty result is not an error)
- Rows show state glyph (✓/✗/●/◌/⊘), workflow, branch · event ·
  duration · relative age; clicking a row opens the run URL.
- Loading, error (first CLI stderr line), and empty states are explicit;
  `↻ Refresh` refetches. picoci is not wired yet: no configured instance
  or API contract was available, so it is deferred rather than guessed.

## Markdown viewer (`src/markdown.rs`, `docs_panel`)

Read-only, reusing the existing GPUI `Markdown` component (the same one
used for transcript rendering). It lists `.md` files under the workspace
directory (depth ≤ 3, skipping hidden dirs, `target/`, `node_modules/`,
capped at 200), renders the selected file, truncates display at 512 KB,
and `↻ Rescan` re-reads from disk. There is deliberately no WYSIWYG
editing: documents stay owned by the editor/terminal workflow, and the
viewer can never corrupt a file.

## Tool launchers (`src/tools.rs`)

Per-workspace `+ bacon` / `+ mbx` buttons appear in the sidebar next to
`+ Shell` / `+ plank`, but only for tools actually found executable on
PATH at startup (here: `bacon` yes, `mbx` not installed, so no dead
button). They open an embedded terminal tab in the workspace directory,
tracked like any other terminal (Fleet minis, confirm-on-close). Adding a
tool is one entry in `known_tools()`.

## Verification

- `cargo test --lib --features test-engine`: 219 passed (ci parsers and
  RFC 3339 math, tool PATH detection, markdown discovery included).
- `cargo fmt --check` and
  `cargo clippy --all-targets --features test-engine -- -D warnings` pass.
- Fresh `/Applications/NPlank.app` installed and relaunched: screenshot
  confirms the sidebar shows `Fleet` / `CI` / `Docs` and a `+ bacon`
  launcher per workspace (no `+ mbx`, correctly, since it is not
  installed). Opening the CI/Docs panels by synthetic click could not be
  verified in this session — synthetic input lands on other
  windows/Spaces (the same blocker seen before; even the previously
  user-verified Fleet menu ignores synthetic clicks here). The toggles
  use the exact wiring Fleet uses; please confirm with a real click on
  `CI` / `Docs` or `Cmd+Shift+I` / `Cmd+Shift+D`.
