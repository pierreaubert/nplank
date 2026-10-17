# Native Direct Manipulation — Design

## User-visible result

Common actions stop being typed commands: drag a file from Finder onto a
session to attach it, drag sessions in the sidebar to reorder them, drag
split dividers between panels, use a native file picker to choose a session
working directory or attachment, and get right-click context menus on
sessions (rename, stop, archive, reveal data dir). Keyboard flows keep
working; the mouse becomes a first-class path instead of a text-selection
fallback.

## Entry points

- [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:2085) the sidebar renders
  sessions as a vertical list; selection and `position` ordering already
  exist in the model ([src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:163)).
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:1287) `Command`
  is the single client→daemon verb set (`create_session`, `prompt`,
  `acknowledge_session`, …). Every new gesture maps onto an existing
  command or a client-side state change — no new verbs for layout actions.
- [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:5965) an attachment
  summary line already exists in the document flow; the prompt composer is
  where dropped files land as attachments.
- There is no file-picker or drag-and-drop call site in `src/ui.rs` or
  `src/native.rs` today — this item is greenfield UI over existing commands.

## Design

1. **File drop → attachment.** Register drop targets on the session view
   and composer. Dropped paths become attachments rendered as chips above
   the composer (name, size, remove button) and are passed to the existing
   prompt submission path. Non-existent or directory drops show an inline
   error on the chip, never a modal.
2. **Sidebar reorder.** Drag handles on session rows write back through
   `Session::position`; persistence rides the existing preference/session
   commands. Drag is visual only until drop (live reflow preview), and
   keyboard reorder (Alt+↑/↓) ships in the same step for accessibility.
3. **Dividers.** Sidebar and activity-panel widths become draggable and are
   persisted with the other window preferences in the data directory.
4. **Context menus.** Right-click on a session row: Open, Stop, Rename,
   Archive, Reveal in Finder (data dir), Copy session id. Every entry is an
   existing command or a Finder reveal via `open -R`.
5. **File picker.** Native open panel for "New session in folder…" and
   "Attach file…", replacing typed paths; the chosen directory becomes the
   session `Backend::Local` working directory through the existing
   create-session command.

## Data changes

None on the wire. Attachment passing uses the existing prompt command
shape; if attachments need a first-class field, it is added as an optional
`#[serde(default)]` list on the prompt command (snapshot-compatible).

## Steps

1. Drop-target plumbing + attachment chips on the composer; tests for chip
   state (add/remove/invalid path).
2. Sidebar drag reorder + Alt+↑/↓ keyboard equivalent.
3. Draggable dividers with persisted widths.
4. Session context menu wiring to existing commands.
5. Open-panel integration for new-session folder and attach actions.

Validate with `cargo test --lib`, clippy, and manual Finder drag tests
(single file, multiple files, folder, missing file).

## Non-goals

Drag between windows, dragging artifacts out to Finder (later; artifacts
panel already offers downloads), and any terminal-emulation view.

## Status

Implemented: file drop onto the conversation (files only, rejects counted
inline), attachment chips with remove + "Attach…" native picker, manifest
injection on submit, Alt+↑/↓ sidebar reorder via `ReorderSession`,
right-click session menu (Open/Stop/Archive/Copy ID), draggable sidebar
divider persisted as `Preferences.sidebar_width`. Deferred: pointer-drag
reorder (keyboard ships instead) and "New session in folder…" picker
(session creation still uses the existing form).
