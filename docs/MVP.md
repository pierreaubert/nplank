# V1 boundaries and recovery

## Ownership

- `src/model.rs`: typed commands, stable identities, observable session states.
- `src/store.rs`: SQLite WAL transactions, ordered transcript chunks, prompt
  request deduplication, bounded 250-entry pages, and restart reconciliation.
- `src/daemon.rs`: bearer-authenticated loopback API, single database-owner lock,
  and independent session workers.
- `src/workers.rs`: persistent console processes, explicit demo execution, and
  Plank v1 remote-control translation.
- `src/agents.rs`: Muse MSP, Codex JSONL, Claude stream JSON, saved provider
  identities, and revision-checked Muse approvals.
- `src/client.rs`: shared asynchronous daemon client.
- `src/ui.rs`: shared macro-built views; native background networking and
  cooperative browser networking use a bounded update queue.
- `src/web.rs`: WASM startup, same-origin connection screen, in-memory credentials.
- `src/web_host.rs`: trusted build assets, path confinement, COOP/COEP headers.
- `src/native.rs`, `src/utilization.rs`: macOS overlay titlebar and independent
  one-second local CPU/GPU sampling with fixed-width indicators.
- `src/bin/`: daemon, typed control CLI, macOS bundler, and WASM packaging utility.

The upstream `plank/` checkout is unchanged. The new application has no TUI
front end. Its console adapter is an MVP integration boundary; extracting a
structured, TUI-independent Plank runtime remains a later step.

## Failure contract

| Failure | Behavior |
| --- | --- |
| Window closes or UI exits | The independent daemon retains execution. |
| GUI attachment disappears | Sessions keep running; the client fetches current state on reconnect. |
| Local engine exits | Only that session fails; other workers continue. |
| Local Stop | The Plank child is killed; tools already executed are not undone. |
| Remote Stop | An interrupt frame is queued; the remote decides when cancellation completes. |
| Daemon restarts | Transcripts remain; interrupted sessions require explicit saved-context resumption or fresh context. Idle durable providers resume their saved identity on the next prompt. |
| Remote connection fails mid-run | No prompt is automatically resubmitted; inspect the remote before continuing. |

Prompt receipts follow a committed SQLite transaction. An identical request ID
and payload are accepted once; conflicting reuse is rejected. Worker generation
checks prevent cancelled workers from overwriting a newly reset context.

This does not guarantee exactly-once tool side effects, survival of every child
tool process, uninterrupted execution across daemon crashes, or automatic KV
restoration. Fresh context retains earlier transcript entries for reference;
it does not feed them back into a newly created local engine.

## Current product limits

- No split panes, tab drag-and-drop, or session deletion. Rename, ordering,
  archive/restore, and group moves use explicit UI actions.
- The composer uses the toolkit's single-line input. A full multiline editor
  and structured Muse question dialogs remain follow-ups.
- Local output uses Plank's console presentation. Fully structured tool cards
  and approvals require the future core/event extraction.
- Remote `/rc` sessions attach when a prompt is submitted. Passive observation,
  robust mid-turn reconnect, rollback/reset translation, and remote context reset
  are not implemented. A remote fresh-context action resets the local adapter;
  the existing remote Plank conversation remains owned by its operator.
- Transcript pages are bounded, but retained database history is not pruned.
- Themes/design presets and live reload are implemented; arbitrary community
  theme import/export and custom design editing are future work.
- macOS was compiled and visually inspected; the shared library also passes an
  iOS simulator compile check. Mobile application hosts must
  remain Rust-only, consistent with the project's source-language requirement.
- Browser builds use the GPUI WebGPU backend, shared-memory WASM, and generated
  wasm-bindgen bootstrap glue. Engines remain on the daemon host. Browser
  credentials stay in memory; only preferences go into local storage.
- Muse Code, Codex, and Claude Code are implemented agent choices. Muse approval
  requests appear with their offered choices. Codex/Claude rely on headless CLI
  permission behavior; their interactive permission prompts are not bridged.
- GPU availability depends on macOS exposing a utilization counter. Unavailable
  or stale measurements display `--%`; the meters are native macOS only.

## Verification

Tests cover durable acceptance, retry conflicts, HTTP authentication and
attachment loss through the real router in memory, restart reconciliation,
workspace moves, bounded paging, replay deduplication, split UTF-8 chunks,
multi-turn console context, and isolation of a native process crash.
Provider fixtures additionally verify exact saved-context resumption, failure
recovery, archive/restore, atomic tab swaps, streamed Muse text without duplicate
completion text, and rejection of obsolete approval revisions. GPU sampling
was exercised locally without privileges, and the macOS titlebar was visually
inspected in the appearance app. The installed Muse CLI also passes the optional
echo-provider smoke test. Real-provider model turns remain a separate manual check.

The sandbox denies listening sockets, so the HTTP integration test uses Axum's
in-memory transport. A native macOS bundle was also launched and its workspace
window rendered. Real model inference and a live remote Plank `/rc` host were
not exercised in this environment. Native automation could inspect the window
but could not deliver clicks, so interactive UI flows remain a manual check.
Android compilation was attempted with `cargo ndk`; it requires an Android NDK
installation that this environment does not currently provide.
Browser checks/builds and same-origin asset/authentication tests are available;
an interactive WebGPU browser session remains a separate runtime verification.
