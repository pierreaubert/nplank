# Native Preflight Guards — Design

## User-visible result

Before an expensive or destructive run starts, the app shows a preflight
panel: target directory and backend, estimated context spend, the tools
expected to need approval, and checkboxes for destructive capabilities
(file writes outside the workspace, network, shell). Nothing runs until
the user confirms. After a run, a summary panel shows what changed and
offers one-click stop/undo entry points. Typed `y` confirmations disappear
from the high-risk path.

## Entry points

- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:64)
  `PendingApproval { id, requirement: serde_json::Value, tool, arguments,
  document, choices }` is the daemon's existing approval contract;
  [src/ui.rs](/Volumes/home_tmp/src/nplank/src/ui.rs:772) already polls and
  renders approvals per session.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:258)
  `Snapshot::contexts: HashMap<session, SessionContext>` exposes
  `ctx_used / ctx_size` — the basis for context-spend estimation without
  new telemetry.
- [src/utilization.rs](/Volumes/home_tmp/src/nplank/src/utilization.rs)
  estimates tokens from transcript bytes; there is no cost model today, and
  none is invented here — preflight reports tokens and context, not dollars.
- [src/model.rs](/Volumes/home_tmp/src/nplank/src/model.rs:100) `Backend`
  (`Demo | Local | Remote | Agent`) is shown verbatim in the panel so the
  user sees which engine and model will run before it starts.

## Design

1. **Preflight panel (client-side).** When a prompt would start a busy run
   (new session, resume, or fan-out to N sessions), compose the panel from
   snapshot state: backend + model, working directory, current context
   fill, sessions affected, and capability checkboxes derived from the
   backend's advertised `EngineControls` features. Confirm sends the
   existing prompt command; Cancel sends nothing.
2. **Approval surfacing.** Pending approvals discovered while the panel is
   open (or later, via the fleet/system-integration attention paths) render
   with tool name, arguments, and the editor document summary
   (`EditorDocument::bytes/sha256`) when present; choices map to buttons.
   Approval never auto-resolves.
3. **Run summary.** On transition `Running -> Idle/Failed`, show a compact
   summary: elapsed time, tokens (utilization estimate), artifacts produced
   (existing `available_artifacts()`), and buttons for Open Report, Stop,
   and Resume where `Session::resumable` allows.

## Data changes

Snapshot-compatible only: an optional `estimated_input_tokens` field on
the prompt command (client-computed, daemon may ignore) so later cost
reporting can compare estimate vs actual. No protocol break; old daemons
accept and ignore unknown fields per existing serde defaults — to be
verified against the command enum's serde attributes before shipping.

## Steps

1. Preflight panel for new-session and resume flows, composed from
   snapshot state; tests for capability derivation from `EngineControls`.
2. Fan-out variant: one panel listing every target session before submit.
3. Approval rendering pass aligned with `PendingApproval` fields.
4. Run summary on busy→terminal transitions, reusing artifacts data.

Validate with `cargo test --lib`, clippy, and a manual destructive-op
drill (shell approval appears, cancel leaves no side effects).

## Non-goals

Dollar-cost estimates, policy engines (org-wide allow/deny lists), and
automatic approval of "safe" tools. Guards inform and confirm; they do
not decide.

## Status

Implemented: `src/preflight.rs` (ctx %, token heuristic bytes/4, warnings)
with 3 tests; submit pauses on risk signals (context ≥ 85%, ~4000+
estimated tokens, attachments ≥ 1 MB) into a "Before you run" modal —
draft and attachments survive cancel. Last-run summary ("Last run Xm ·
N tokens generated") appears after busy→idle transitions. Approvals keep
using the existing `PendingApproval` UI. The optional
`estimated_input_tokens` wire field was not added (client-only estimate).
