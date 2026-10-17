# Native Embedded Terminal — Design

## User-visible result

The app gains terminal tabs. A tab runs any local program attached to a
real PTY — the `plank` TUI, a shell, `nplankctl`, build tools — rendered
with the app theme's font and colors, with working alternate screen, mouse
reporting, bracketed paste, resize, and scrollback. Quick-launch buttons
start `plank` or a shell; tabs live beside session views, so the user can
drive the TUI and the native UI in one window.

## Entry points

- `alacritty_terminal` 0.26 (crates.io) provides the terminal brain: VT
  parsing, cell grid, scrollback, selection — pure Rust library code, no
  renderer. Zed (also GPUI) ships its terminal on this crate.
- `portable-pty` 0.9 (crates.io, from WezTerm) spawns processes on a PTY
  with size control and read/write handles.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:1287) `Command`
  and the client in [src/client.rs](/Volumes/home_tmp/src/nplank/src/client.rs)
  stay untouched: terminals are local processes, exactly like the
  client-only slot used by NATIVE-DIRECT-MANIPULATION.md.
- `WorkspaceView` ([src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:273))
  gains a tab kind; the macOS entry
  ([src/native.rs](/Volumes/home_tmp/src/nplank/src/native.rs)) owns PTY
  lifecycle. iOS cannot spawn subprocesses, so this is desktop-only by
  platform rule, like the iOS connect-only client.

## Design

1. **Terminal core (`src/terminal.rs`, feature-gated `terminal`).** Owns
   one `portable_pty` pair plus one `alacritty_terminal::Term`. A reader
   thread feeds PTY output into the parser and nudges GPUI; writes
   (keystrokes, paste, resize via `PtySize`) go through the PTY master.
   Process exit marks the tab exited; the tab is never auto-closed.
2. **Rendering.** A custom GPUI element paints visible grid rows: cell
   runs batched by style (fg/bg, bold, underline), cursor block, selection
   highlight. Font metrics come from the active theme so the terminal
   matches the rest of the app. Damage-limited repaint: only rows the
   parser flags as changed re-render.
3. **Input.** GPUI key events translate to terminal byte sequences
   (arrows, function keys, Ctrl combos, Alt-as-meta), keypad application
   mode honored from parser state. Paste uses bracketed-paste markers
   when the child enables them. IME composition commits as text writes.
4. **Tabs.** `TerminalTab { id, title, cwd }` created from the sidebar
   ("New Terminal", "Open plank TUI"), listed next to sessions, closable,
   title updated from the child's OSC title when present.
5. **Scrollback and search.** Scroll via wheel/trackpad into parser
   scrollback; find-in-scrollback is v1.1 (see Non-goals).

## Data changes

None. No daemon, protocol, or snapshot involvement; terminal state is
never persisted except tab cwd in window preferences.

## Steps

1. Add `alacritty_terminal` + `portable-pty` behind a `terminal` feature;
   `src/terminal.rs` core with headless tests (feed recorded VT output,
   assert grid contents).
2. GPUI grid element + damage-limited repaint; theme font/colors.
3. Keyboard/paste/IME input mapping with unit tests for key sequences.
4. Tab UI: create/close/switch, quick-launch `plank` and `$SHELL`.
5. Resize, scrollback scrolling, selection + copy/paste round-trip.

Validate with `cargo test --lib`, clippy, and manual drills: run `plank`
(full-screen TUI with mouse), `vim`, `htop`; resize mid-run; copy/paste
both directions; large output (`yes | head -c 10M`) without UI jank.

## Risks

Terminal correctness is a long tail (rare escape sequences, wide chars,
emoji width). `alacritty_terminal` bounds the risk — parsing is its job —
but rendering edge cases will surface in the manual drills; log unknown
sequences in debug builds rather than failing visibly.

## Non-goals

libghostty embedding (Zig core behind a C API; its rendering surface
conflicts with GPUI's compositor), find-in-scrollback and shell
integration (OSC 133 marks) in v1, terminal tabs on iOS/web, and making
the terminal a session backend — daemon sessions and terminals stay
separate worlds that share a window.

## Status

Implemented: `src/terminal.rs` (portable-pty + alacritty_terminal, default
palette fallback, key encoding, bracketed paste, scrollback) with 6 tests
including a live PTY round-trip; GPUI tab panel with Menlo grid rendering,
cursor inversion, resize-from-viewport, sidebar "+ Shell" / "+ plank"
launchers, scroll-wheel scrollback. Not yet visually verified; find-in-
scrollback and mouse-reporting forwarding remain future work.
