# Native Rich Transcript — Design

## User-visible result

Tool calls and results render as structured, collapsible blocks instead of
raw text: a folded header (tool name, target, duration), syntax-highlighted
code and diffs, real tables for tabular output, inline artifact previews
(images, HTML, downloads), and jump-to-error bookmarks. The transcript
becomes a reviewable record rather than a scrollback buffer, while plain
assistant text keeps streaming exactly as today.

## Entry points

- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:224) `Entry` is
  `{sequence, session_id, role: String, text: String}` — role is free-form
  text and `text` is uninterpreted, so structure is unavailable today.
- [src/transcript.rs](/Volumes/home_tmp/src/nplank/src/transcript.rs:32)
  `messages()` coalesces adjacent same-role chunks into `Message`s; rendering
  decisions belong here or in a sibling grouping module.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:273)
  `EngineTelemetry::available_artifacts()` already lists downloadable
  exports (transcript, insights, preview) shown by the artifacts panel at
  [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:5391).
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:64)
  `PendingApproval` carries `tool`, `arguments`, and an optional
  `EditorDocument { bytes, sha256 }`; document text is fetched separately —
  the existing precedent for structured side-data that snapshots stay lean.

## Design

1. **Structured entries (snapshot-compatible addition).** Add optional
   fields to `Entry` with `#[serde(default, skip_serializing_if = "Option::is_none")]`:
   `kind: Option<EntryKind>` where `EntryKind` is a small tagged enum
   (`Text | ToolCall { name, target } | ToolResult { ok } | Diff { path } |
   Artifact { name, media_type }`). Old daemons and old clients keep working:
   missing `kind` means `Text`, exactly today's rendering.
2. **Client-side rendering.** Extend the grouping in `src/transcript.rs` so
   messages keep their `kind`; `WorkspaceView` renders per-kind blocks:
   collapsible tool blocks (folded by default for results over a size cap),
   diff blocks with +/- gutter coloring, artifact chips that open the
   existing artifacts download/preview path, and error bookmarks in a
   transcript minimap strip.
3. **Syntax highlighting** reuses whatever highlighter the editor surface
   already loads; if none exists, plain monospace inside the structured
   block ships first — structure before color.

## Data changes

- One optional `kind` field on `Entry` (additive, defaulted). The daemon
  fills it from engine events it already emits as text; text stays the
  source of truth so terminals and the web client are unaffected.
- No new endpoints. Artifact bytes keep flowing through the existing
  downloads client in `src/client.rs`.

## Steps

1. `EntryKind` in `src/model.rs` + serde round-trip tests (absent ⇒ `Text`).
2. Daemon fills `kind` for tool calls/results/diffs at emission sites.
3. Grouping carries `kind` through `Message`; per-kind renderers in `ui.rs`
   with collapse state keyed by `session_id + sequence range`.
4. Error-bookmark strip and click-to-jump; verify with `cargo test --lib`,
   clippy, and side-by-side comparison of old vs new transcript for the
   same recorded session.

## Non-goals

Per-hunk approve/reject for non-editor diffs (editor approvals already
exist via `PendingApproval`), rich rendering inside the TUI/web clients,
and editing code from transcript blocks.

## Status

Implemented: `EntryKind` (model), schema v4 `kind` column with migration
(old rows stay plain text), daemon ingest fills `ToolCall` from engine
tool events (main + aside paths), grouping respects kinds, conversation
renders per-kind headers and tinted diff lines. `ToolResult`/`Artifact`
kinds exist in the model; the engine does not emit them yet. Existing
test `inline_editor_migration…` now asserts schema version 4.
