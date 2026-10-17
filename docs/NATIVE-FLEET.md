# Native Fleet View — Design

## User-visible result

Opening the app shows every live session as a compact card in a wrapping
grid: name, run state, current activity, token rate, context fill, elapsed
time, and attention flags. Cards update on the existing snapshot poll.
Clicking a card opens that session. Sessions needing attention (approval,
failure, recovery) sort first and are visually distinct, so a stuck run is
visible at a glance instead of by visiting each session.

## Entry points

- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:248) `Snapshot`
  already carries `sessions: Vec<Session>`, `telemetry: HashMap<session,
  EngineTelemetry>`, and `contexts: HashMap<session, SessionContext>`.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:139) `Session`
  exposes `state: RunState` (`Idle | Starting | Running | Disconnected |
  Interrupted | Failed`), `attention: bool`, `approvals: Vec<PendingApproval>`,
  `recovery_required: bool`, `archived: bool`, `started_at`, `completed_at`,
  `position`, `workspace_id`, and `backend`.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:1257)
  `EngineStatus::status_line()` already formats `state · Context used/size ·
  tok/s · Think …` — the card's activity line reuses this verbatim.
- [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:557) the existing poll
  loop refreshes snapshots; no new timers. The sidebar at
  [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:2085) is today's
  per-workspace vertical list and already owns session selection.

## Design

1. Add a pure derivation `fleet_cards(&Snapshot) -> Vec<SessionCard>` in
   `src/ui.rs` (or a small `src/fleet.rs` if size demands). Fields: `id`,
   `name`, `workspace_id`, `state`, `status_line` (via
   `EngineStatus::status_line()`), `ctx_fraction` from `contexts`
   (`ctx_used / ctx_size`), `elapsed` from `started_at` rebased by
   `Snapshot::server_time_ms`, and `attention: Option<AttentionReason>`
   (`Approval | Failed | Recovery | Disconnected`).
2. Sort: attention first, then `RunState::busy()`, then `position`. Archived
   sessions are excluded; archived filtering already exists elsewhere.
3. Add overview state to `WorkspaceView`: `show_fleet: bool` plus a toolbar
   toggle. The app opens in fleet view when a stored preference says so, or
   when two or more sessions are busy at launch.
4. Render cards with fixed height so streaming text cannot reflow the grid;
   2–4 columns chosen from window width, one column in compact/mobile mode.
   Reuse toolkit macros (`layout!`, `Hd`, `Tx`) and theme tokens, matching
   the existing session-preview panel.

## Data changes

None. Everything derives from the current `Snapshot`; no protocol or daemon
change, no compatibility risk.

## Steps

1. `SessionCard` + `fleet_cards()` with unit tests (sorting, attention
   precedence, ctx fraction, elapsed formatting).
2. Overview state + toggle in `WorkspaceView`; card click selects the session
   through the existing selection path and exits overview.
3. Empty state ("No live sessions — create one") and loading state.
4. Persist the open-in-fleet preference next to existing preferences; verify
   with `cargo test --lib`, `cargo clippy --all-targets -- -D warnings`, and
   the UI QA harness (`src/bin/nplank-ui-qa.rs`).

## Non-goals

Per-session sparklines, drag-to-reorder cards, keyboard grid navigation,
and iOS-specific layout variants are out of scope for the first slice.

## Status

Implemented: `src/fleet.rs` derivation (5 unit tests), overview toggle in
the sidebar plus View-menu action (Cmd+Shift+F), card grid with attention
ordering, ctx bar, elapsed time, click-to-select. Card grid rendering is
covered by unit tests and compiles clean; it has not been visually
screenshotted (synthetic clicks do not reach the app on this machine).
Open-in-fleet preference persistence is not wired yet.
