# Native System Integration — Design

## User-visible result

The app behaves like a macOS citizen: when a run finishes or needs input
while the app is in the background, a Notification Center banner appears;
clicking it focuses the window on that session. The Dock icon shows a badge
count of sessions awaiting attention. A menu-bar item mirrors live status
(active runs, tok/s) and offers quick actions (open app, stop all). The
`/pair` QR, daemon watchdog, and preferences live where macOS users expect
them — menus and panels, not typed commands.

## Entry points

- [src/notifications.rs](/Volumes/home_tmp/src/nplank/src/notifications.rs:12)
  `Notification { id, session_id, generation, title, body, mode, expires_ms }`
  already arrives in every `Snapshot` and is rebased against
  `server_time_ms`; [src/notifications.rs](/Volumes/home_tmp/src/nplank/src/notifications.rs:189)
  already posts via `notify-rust`. The module's own contract: notifications
  never enter transcripts or acknowledge attention.
- [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:678) `poll()` already
  samples utilization meters on macOS (`self.meters`) — the same sample feeds
  the menu-bar readout.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:144) `Session::attention`
  plus `approvals`/`recovery_required` define "awaiting attention" for the
  Dock badge; the fleet design (NATIVE-FLEET.md) shares this definition.
- [src/main.rs](/Volumes/home_tmp/src/nplank/src/main.rs) the daemon
  watchdog (`supervise_daemon`) already keeps runs alive while the window
  is closed; system integration is the visible half of that promise.

## Design

1. **Notification actions.** Extend the existing notify path with a click
   handler that activates the app and selects `Notification::session_id`.
   Focus policy stays as designed: no banner for the session the user is
   already viewing, 30 s expiry, dedup by `id + generation`.
2. **Dock badge.** In the poll loop, count sessions with `attention ||
   !approvals.is_empty() || recovery_required`; set the Dock tile badge to
   that count (empty at zero). No new data.
3. **Menu-bar item.** A status item showing active run count and aggregate
   tok/s from the existing meters sample, with a small menu: Open NPlank,
   Stop All Runs (existing stop command per busy session), Quit. The item
   is created once in the platform layer (`src/native.rs` on macOS), not in
   shared GPUI code.
4. **App menu.** Standard macOS menus: Settings… opens the existing
   preferences surface, Pair iOS Device… opens the existing `/pair` QR
   panel as a menu command, Check Daemon Status shows watchdog state.

## Data changes

None. All state derives from `Snapshot` and the poll loop; notification
click-through is process-local.

## Steps

1. Notification click → focus-and-select session; keep the no-banner-when-
   focused rule and cover it with tests on the focus policy.
2. Dock badge from attention count in `poll()`.
3. Menu-bar status item + quick actions in the macOS platform layer.
4. App-menu entries for Settings and Pair iOS Device.

Validate with `cargo test --lib`, clippy, and manual background tests:
start a run, switch away, confirm banner + badge; click banner, confirm
the right session is focused; finish all runs, confirm badge clears.

## Non-goals

Notification Center custom actions (Approve/Deny inline) — approvals stay
in-app until their audit story exists. Windows/Linux tray parity and iOS
push are separate efforts.

## Status

Implemented: Dock attention badge (`src/dock.rs`, objc2, changes only),
App-menu "Pair iOS Device…", View-menu "Fleet Overview" (Cmd+Shift+F).
The menu-bar status item and notification click-to-focus are implemented
in `crates/macos-shim` — the one crate allowed `unsafe` — so the main
crate keeps `forbid(unsafe_code)`. The status item shows
`nplank · N running · M need attention` with Open / Stop All Runs / Quit
actions drained by the UI poll loop; bundled builds post through
UNUserNotificationCenter with the session id in `userInfo`, and clicking
a banner selects that session (unbundled dev binaries keep notify-rust).
Fleet toggling now also closes an open terminal panel, which otherwise
outranks Fleet in the content stack and made the toggle look dead.
