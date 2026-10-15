# nplank

A Rust GPUI application for persistent Plank workspaces. The window is a client;
an independent Rust daemon owns execution and SQLite owns durable state.

## Checkout

`plank/` is a submodule pointing at the author's Plank fork on GitHub (its
changes are not merged upstream yet), so clone with submodules:

```sh
git clone --recurse-submodules git@github.com:pierreaubert/nplank.git
```

If you already cloned without them, fetch afterwards:

```sh
git submodule update --init --recursive
```

The `plank/` recipes (`just build-plank`, the UI QA probe's default engine
path) expect that checkout to be present.

## Run

Keep `gpui-toolkit` beside this directory. This v1 uses its source-beta crates
and the matching GPUI dependency recorded in `Cargo.lock`.

```sh
cargo build
cargo run --bin nplank
```

The app starts `nplankd` automatically and reconnects on subsequent launches.
State defaults to this checkout's `.nplank/`; override it with `--data-dir` or
`NPLANK_DATA_DIR`. Closing the window leaves the daemon and its runs alive.
If an older daemon is already running, finish its runs and restart it with the
updated binary. The current client/daemon protocol is version 44; startup reports
an incompatible daemon rather than replacing a live service.

On macOS, File → New Workspace (Cmd-Shift-N) creates a workspace; File → New
Agent (Cmd-N) opens agent creation. The workspace's right-hand **+** opens the
same popup with that workspace selected and local Plank as the default.
Muse Code, Codex, Claude Code, remote Plank, and demo are also available.
Workspace names select the destination; the right-hand arrow collapses a group.
Preferences opens with Cmd-,; themes and designs also have View menus.

The conversation occupies the main pane. Tool output and reasoning fold into
expandable rows. **Details** exposes agent/workspace renaming, tab ordering,
archive/restore, and moving stopped sessions between workspaces. Moving starts
a fresh engine context and preserves the transcript. Drafts, selected tabs,
and collapsed groups survive relaunch.
Archiving requires a stopped agent and preserves its transcript.

## Agent providers

Executables and login/configuration belong to the daemon host. The picker shows
detected paths, including `~/.local/bin/muse`; **Advanced** permits another
executable and a JSON array of provider arguments. Model selection is optional.
The app retains each provider's sandbox and permission settings.

- **Muse Code:** `muse serve` over its versioned stdio protocol, streaming text,
  tools, and explicit approval choices. Approval buttons carry the current
  requirement revision; stale choices are rejected.
- **Codex:** `codex exec --json`, resuming the exact saved thread ID on later turns.
- **Claude Code:** `claude --print --output-format stream-json --verbose`,
  resuming the exact saved session ID on later turns.

Saved provider identities persist alongside transcripts. After interruption,
choose **Resume saved context** when available or **Fresh context**. Neither
action replays an interrupted prompt. Fresh context retains transcript history.
Ephemeral Muse sessions cannot resume after their host exits. Codex/Claude use
headless CLI permission behavior; interactive permission prompts are not bridged.
Muse structured question dialogs are not yet supported; answer them in Muse.

## macOS window meters

The native titlebar shows CPU and GPU icons with reserved-width, monospaced
percentages. A background sampler updates once per second; engine work and UI
rendering do not wait for it. CPU is whole-machine usage across cores. GPU uses
the busiest accelerator's `PerformanceStatistics` utilization counter, following
[macpow's approach](https://github.com/k06a/macpow/blob/main/src/metrics.rs).
Readings always describe the local Mac, including when using a remote daemon.
Missing or stale readings show `--%`. No elevated privileges are needed.

For a model-free smoke check against a running daemon:

```sh
cargo run --bin nplankctl -- demo --directory .
cargo run --bin nplankctl -- snapshot
```

Demo sessions are explicitly labeled and do not invoke a model.

## Browser / WASM

The browser uses the same Rust GPUI workspace view and authenticated API.
Agents, executables, working directories, and SQLite live on the daemon host;
the browser never launches engines or owns a running session.

```sh
just wasm-setup   # Install nightly, rust-src, and matching wasm-bindgen CLI
just wasm-check   # Check the shared UI for wasm32-unknown-unknown
just wasm-build   # Build release WASM and package assets into target/web
just wasm-serve   # Build, then serve the browser and API from one daemon
```

Open `http://127.0.0.1:47831` and paste the token from the daemon's data directory
(`.nplank/token` by default). The token stays in page memory; themes and selection
use browser local storage. Reloading requires authentication again. Stop an
existing daemon before using `wasm-serve`, because it owns the same port and
database lock. To serve an already-built client without rebuilding:

```sh
cargo run --no-default-features --bin nplankd -- --web-dir target/web
```

GPUI web requires WebGPU, shared WASM memory, and a secure browser context
(HTTPS or localhost). The daemon supplies COOP/COEP isolation headers. Use a
same-origin HTTPS reverse proxy or an SSH tunnel for a remote daemon; the service
continues to bind only loopback. Separate-origin API access is not enabled.
Native builds retain stable Rust; web builds select nightly to rebuild the
standard library with atomics. Override it with `NPLANK_WASM_TOOLCHAIN`.
Application behavior remains Rust; packaging generates the HTML loader and
wasm-bindgen JavaScript/worker glue required by browsers.

## Platform commands

| Target | Check | Build |
| --- | --- | --- |
| macOS ARM64 | `just check-macos` | `just build-macos` |
| Linux | `just check-linux` | `just build-linux` |
| Windows | `just check-windows` | `just build-windows` |
| iOS | `just check-ios` (simulator) | `just build-ios` (device library) |
| Android | `just check-android` | `just build-android` |
| Browser | `just wasm-check` | `just wasm-build` |

Desktop cross-compilation requires the target's native SDK and linker; run its
recipe on the matching host if these are absent. `NPLANK_LINUX_TARGET` and
`NPLANK_WINDOWS_TARGET` select another architecture. Android requires `cargo-ndk`
and an installed NDK. Mobile recipes build the shared library, not a launchable
IPA or APK; Rust mobile hosts and app packaging are still pending.

## Local Plank

Build the existing `plank/` checkout or use an installed Plank executable. Enter
its absolute path and engine arguments as a JSON array, for example:

```json
["--model", "gemma4-e4b"]
```

The daemon starts a persistent `plank --ui console` process per local session.
It uses Plank's `+DWARFSTAR_WAITING` marker to detect readiness and completed
turns, and `+PLANK_BUSY` for work started by another controller. Prompts go over
stdin; UTF-8 output streams into the transcript.
Do not include `--ui` or `--prompt` in the engine arguments.

Rebuild the bundled engine with `just build-plank` to use slash commands and
structured questions and editors; older installed Plank binaries lack these console changes.
Use the resulting executable's absolute path in the agent popup. Type `/`, use
the composer's `/` button, or press Cmd-Shift-P to discover controls. Selecting
a command prepares the composer for explicit submission. Plank questions and
yes/no confirmations appear as compact answer controls, including multiple
selections and decline. The picker loads installed extension commands from the
local engine. File, memory, and profile commands open paged editors with explicit
Save and Cancel actions. Original documents load once per revision through bounded,
authenticated chunks; polling snapshots contain metadata. Recovered drafts are
preserved, and failed original loads offer Retry and Cancel.
Long input rows display a bounded window near the caret; ellipses indicate hidden
text, while selection and clipboard operations retain the complete value.
Compact document history keeps small edits to long rows, splits and joins undoable
within the existing history budget, including after draft recovery.
Engines advertising chunked Saves accept edited drafts
up to 32 MiB; older engines retain a 256 KiB Save limit. Opening existing files
still has a 256 KiB limit. Oversized drafts remain editable with Save disabled.
Browser drafts and undo history use IndexedDB; failed writes show Retry save.
`/engines` opens a compact chooser; downloads show progress
and offer leave/cancel controls. Profile edits offer a restart that restores the
saved session. Engine selections persist for future fresh contexts. See [control coverage](docs/PLANK-CONTROLS.md) for the
remaining TUI-only features and verification requirements.

Activity → Model → Downloads shows live model-update progress and the last
result. Download/resume and cancellation controls prepare commands for explicit
Send; cancellation can keep or delete partial files. Verified artifacts are
preserved. Rebuild Plank to receive these snapshots. The client/daemon protocol
is version 44; finish active runs before restarting an older daemon. Database
schema 3 migrates inline editor originals into separate chunk storage atomically.

Activity → Agents shows delegated tasks, running/finished state, elapsed time,
and completed-pass input/output tokens. Report expands a bounded Markdown result
in the same panel; Copy report copies that displayed text. Stream displays the
newest 4 KiB of rendered child output with Copy stream tail and a clipping notice.
GUI child text stays separate from the parent transcript stream. Rebuilt engines publish serial and
fan-out roster updates during prefill/generation, at most once per second plus
phase transitions. In-flight streamed-piece counts remain separate from completed
token totals. Repeated concurrent definitions get separate roster labels. Finished denotes a run
that ended; its final report describes success, failure, or interruption.

Activity → Model → Metrics shows context estimates, numeric usage, and recent
decode/prefill charts. Rebuilt engines refresh category estimates between passes,
resident usage during generation, and usage totals when returning idle. Speed
samples update once per second while generating. Load/refresh still prepares
`/context`, `/usage`, or `/toks` for Send; older engines retain text reports.

Activity → Model → Remote shows an existing Plank host's endpoint, controller,
and pending grant requests. Report prepares `/rc status`; Grant prepares the
matching client ID for explicit Send. Rebuilt hosts publish policy updates
without their bearer token. Connect as observer attaches without a prompt;
Request control and Release control change the attachment role. Rebuilt hosts
support observer authentication. Submitted input waits locally for a grant.
Local GUI hosting remains incomplete.

Multiple tabs do not bypass native engine limits. The DeepSeek backend's GPU
lock can prevent several local model processes from running simultaneously.
Use provider arguments or Plank's shared remote inference host when needed.

## Remote access

There are two independent connection modes:

1. **Existing Plank `/rc`:** start `/rc` in a running Plank, tunnel its port
   over SSH, and create a remote session using the loopback WebSocket URL.
   Set its token environment variable **before starting nplankd**, for example
   `PLANK_REMOTE_TOKEN`. Credentials are not stored in session metadata.
   Connect as observer attaches to rebuilt hosts without sending input. The
   first prompt can also connect, authenticate, replay output, and request
   control; its input stays local until granted. Completed turns retain the authenticated attachment for idle
   updates and subsequent inputs. An unexpected disconnect requires recovery;
   uncertain inputs are not automatically replayed. Replayed event IDs
   are deduplicated. The remote must grant control to accept prompts.
   Slash controls require the rebuilt remote engine's command-completion
   and identified command-completion capabilities; older engines are rejected for controls instead of waiting
   indefinitely. Rebuilt hosts support remote question forms and text editors;
   ordinary prompts require their identified-completion capability, so another
   host turn's notification cannot finish your input. Prompts wait for the host's
   idle dispatcher and complete only on their matching request ID.
   remote engine/profile restart, HTML previews, and remaining modal paths are
   incomplete.
2. **Remote nplank daemon:** run `nplankd` on the other host, tunnel its
   loopback port, and launch the app with `--connect http://127.0.0.1:PORT`.
   Supply that daemon's token through `NPLANK_TOKEN`, or select another variable
   with `--token-env`. This mode exposes all its workspaces and sessions.

Neither daemon binds a public network interface. The token is stored in a
private local file. `nplankctl command FILE.json` submits the same typed commands
used by the GUI, including stop, reset, move, and rename.

## Appearance mini app

```sh
cargo run --bin nplank -- --appearance
just bundle-appearance-macos   # Standalone Finder-launchable mini app
```

The appearance view previews real toolkit components and independently cycles
14 theme variants and seven design systems. Changes are saved atomically and
reload in other open app windows. The main window also exposes **Appearance**.

Views extensively use `layout!`; component colors use `ComponentTheme`.
`just components`, `just themes`, and `just toolkit-check` use the toolkit CLI
for component discovery, theme discovery, health checks, and layout validation.
Set `GPUI_TOOLKIT_CLI` if its executable is elsewhere.

## Checks and packaging

`just qa-ui` opens the real native client and daemon with a temporary database,
workspace and engine home. It uses bundled Plank's `--fake-gpu` engine, so no
model weights load or download. The Plank tab exercises `/command`, settings,
`/forget qa-confirmation` approval and `/open example.rs`; the Transcript sample tab exercises context
folding. Notifications start disabled. Closing the window or pressing Ctrl-C
stops this launcher's sessions and daemon and removes its temporary files.

To prepare files and check the engine dispatcher without opening a socket or
window, run `cargo run --features test-engine --bin nplank-ui-qa -- --prepare-only`.
This retains the fixture directory for inspection. The full launcher needs a
normal desktop terminal; restricted execution sandboxes may deny its listener.
Use `--keep-data` when retaining a full verification run for inspection.

```sh
cargo fmt --check
cargo clippy --all-targets --features test-engine -- -D warnings
cargo test --features test-engine
just smoke-muse   # Installed Muse, model-free echo provider, memory-only session
cargo run --bin nplank-package
```

The last command creates an unsigned local macOS bundle at
`target/debug/nplank.app` after `cargo build`. `cargo test --no-default-features
--features test-engine` verifies the service without compiling the UI.

The application has been built and rendered on macOS, and its shared Rust library passes
`cargo check --target aarch64-apple-ios-sim --lib`. Linux and Windows backends are
selected through `gpui-miniapp`, but have not been built here. The shared UI is
responsive and the toolkit has mobile backends; **Android/iOS launch hosts,
packaging, and device verification are not implemented**. No Swift, Kotlin,
Java, Python, or JavaScript application logic has been introduced. Browser
packaging generates standard HTML and JavaScript bootstrap/worker glue.

See [MVP boundaries and recovery](docs/MVP.md) for the exact resilience contract.
