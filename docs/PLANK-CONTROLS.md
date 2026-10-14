# Compact Plank Controls

The goal is full Plank control coverage in native and browser clients, with
slash commands preserved. Coverage means working behavior, not a catalog entry.

## Interaction

- Keep the session view compact: transcript, composer, and a small `/` button.
- Typing `/` shows searchable command suggestions above the composer.
- Cmd-Shift-P or View → Plank Commands opens the full picker on macOS.
- Picking a control prepares its command. Users supply arguments and explicitly
  submit it; picking destructive controls must not execute them immediately.
- Preserve custom skill, template, and named sub-agent commands verbatim.

## Implemented

- Activity → Agents exposes the shared terminal subagent roster in the existing
  160-pixel scroll area. Tool delegations, named fan-out passes and explicit
  `/subagent` commands publish task, running/finished state, elapsed milliseconds
  and completed-pass token totals. Re-bracketing a running fan-out label resumes
  its row, preserving spend. Active rows precede finished rows; snapshots cap at
  64 entries with bounded Unicode labels/tasks and full retained/running counts.
  Context reset clears the source roster and client telemetry. Local console and
  remote adapters share validated metric frames, generation fencing and replay
  coalescing. The Delegate action prepares `/subagent` for explicit Send.
  Client/daemon protocol 28 carries stable run identities, live pass counters, rendered tails and optional bounded
  final reports. Report expands Markdown within the same panel; Copy report copies
  the displayed text. Explicit commands, agent tools and fan-out retain the child
  result or an error summary separately from the parent continuation. Reports cap
  at 4 KiB, remove terminal control characters and show a truncation notice; complete
  results retain the original parent-conversation path. Repeated labels keep distinct
  identities across completed runs and context resets. Older roster snapshots omit
  report controls. Model-free tests cover report clipping, identity, malformed frames
  and command reports without parent follow-up text. Quiet passes, explicit commands
  and parallel slots publish prefill progress and streamed-piece counts at most once
  per second, with immediate phase transitions. Callbacks bind stable run identities;
  finished/reset runs reject late updates. Completed-pass totals clear live estimates,
  and explicit commands credit each pass without adding totals again at completion.
  Concurrent repeated definitions receive unique batch labels without changing their
  instructions or engine selection. Live snapshots refresh elapsed time during engine
  callbacks. In-flight piece counts are not billed tokens; providers can group text.
  Model-free regressions cover coalescing, stale ownership, separate main status,
  duplicate labels, actual command accounting and invalid progress frames.
  Stream displays the newest 4 KiB of rendered visible/thinking/tool/error output
  in the same panel, with a clipping notice and Copy stream tail. Report/Stream
  actions share one compact row and only one detail view expands at a time.
  Renderer thinking/tool settings apply before capture. Output mirrors coalesce
  callbacks and flush the final chunk; stable identities reject stale output.
  GUI console delegations suppress duplicated primary text, and remote child text
  stays out of main replay. Regular CLI primary bytes remain unchanged. TUI-owned
  explicit child turns route through their subagent sink. Memory-pass output stays
  silent. Model-free tests verify clipping, retirement, thinking visibility,
  final flushing, primary-byte parity and main/child separation.
  Post-tool activity, edit cards, task-completion lines, hook warnings, tool status
  and Markdown plans now reach the same child stream. Serial, explicit-command and
  fan-out batches scope their sinks to the active child and restore parent callbacks
  afterward. Edit cards render without ANSI escapes; notices retain the bounded tail.
  GUI and remote child notices avoid main transcript replay, while unobserved CLI
  callback ownership is preserved. Model-free regressions exercise an actual edit,
  retained plans/warnings, parent sink restoration and unchanged delegation reports.
  Full archived child histories, elapsed ticks while blocked in tools and actual engine/native/browser
  interaction remain to implement or verify. Finished does not assert success.
- Rebuilt engines publish context categories at generation boundaries and refresh
  context, numeric usage and speed rings automatically when returning idle.
  The context view follows the existing live resident counter, keeping category
  estimates fixed during a pass instead of repeatedly tokenizing the transcript.
  Console speed snapshots share the one-second status cadence; TUI speed snapshots
  also sample at most once per second. Remote replay coalesces each category
  independently. Refresh commands remain available for older engines. Model-free
  tests cover idle refresh without commands, transcript preservation, updated
  totals, bounded streaming samples and replay retention. Real engine/native/browser
  interaction remains to verify.
- Templates declare up to 32 bounded variable names in the runtime catalog.
  Choosing a parameterized template from the picker or Extensions opens its
  compact form. Single-line values, including explicit empty values, are quoted
  into the composer for review and explicit Send. Quoted arguments preserve
  literal quotes and backslashes; unquoted Windows paths retain their previous
  behavior. Parameter names are searchable. Form values stay in UI memory and
  are scoped by session, worker generation and template; only the prepared
  command becomes a saved draft. Catalog validation rejects invalid/duplicate
  names and parameters attached to other command kinds. Engines without the
  metadata retain free-form commands. Client/daemon protocol 24 carries the
  declarations. Real native/browser form interaction remains to verify.
- Tasks & goal shows up to 64 structured task rows, prioritizing active and
  pending work, with a bounded goal summary. Start, Done, Pending and Reopen
  prepare status changes; the row's ellipsis exposes subject and active-label
  editing. Add task prepares a new subject. Full report retains the complete
  requested text view within the same 160-pixel scroll area. Every mutation
  requires explicit Send and the rebuilt engine's advertised command syntax;
  busy sessions and read-only observers cannot edit. `/tasks add`, `set`,
  `rename` and `active` share the plain/TUI implementation. IDs remain
  append-only, and task edits atomically save before success is acknowledged,
  without adding model prompts. Replies are one-line receipts; failed saves
  explicitly retain dirty in-memory changes. Task-only conversations can be
  saved and resumed, with titles derived from their first task; global retitling
  preserves task-based titles. Bare `/tasks` remains read-only.
  Local/remote snapshots validate IDs, statuses, counts and label bounds, retain
  generation fencing, and coalesce in replay. Client/daemon protocol 24 carries
  the rows; older engines retain report-only behavior. Real native/browser and
  engine interaction remain to verify.
- Insights scans, model-written sections and HTML rendering now observe the
  owning remote controller's cancellation, including identified command stops
  after the transient interrupt flag clears. Scans poll before listing and
  after progress callbacks, including the final callback and empty histories.
  Late cancellation discards a returned section and skips retries and later
  sections, whether the engine returns success or a cancellation error. A stop
  honored during prose still saves completed statistics; a new stop during
  rendering preserves the previous report. Shared stops remain until dispatch
  retirement. Model-free regressions cover both aside tiers, late success/error
  returns, identified inputs, scan/render stops and clean retry. Real engine and
  controller interaction remains to verify.
- Model → Metrics → Insights displays engine-authored statistics and optional
  written sections in the existing 160-pixel scrolling panel. Load prepares
  `/insights fast` for statistics without model writing; Written insights uses
  `/insights`, and Rewrite uses `/insights fresh`. Submission opens the matching
  view. Tables show tools, languages, projects, friction, reply times and failure
  rates; host-local activity hours appear only with representative timing.
  Prose and multiline setup snippets accompany the figures. A completed HTML
  write publishes a Markdown snapshot through local and remote metric adapters,
  with generation fencing and independently coalesced replay. Lists show up to
  32 entries; snapshots cap at 64 KiB with a truncation notice. The complete HTML
  remains on the engine host. Client/daemon protocol 24 carries the new report;
  older engines retain their transcript summary. Rich HTML preview, chart parity
  and real engine/native/browser interaction remain to verify.
  The complete standalone HTML is also transferred after its successful host
  write. Insights shows the same compact Download / Save file row used for
  conversation exports. Artifact categories keep the latest report and latest
  conversation export independently; selecting a report cannot download a
  conversation merely because it has the same filename. Earlier frames default
  to conversation exports. Browser file cleanup recognizes both categories.
  The compact report snapshot includes the HTML checksum, so Download is shown
  only for the matching completed report. A rewritten statistics view cannot
  silently offer the previous report's file while the new transfer is pending.
  Verified descriptors are reconstructed from the daemon's file registry after
  live telemetry is cleared during reattachment. This preserves downloads without
  reviving stale status or counters. With no current statistics snapshot, Insights
  offers the last complete report; with a current snapshot, checksum matching
  remains required. Generation changes and context resets still revoke old files.
  Model-free tests compare transferred reports byte-for-byte with the files
  written by `/insights fast`, and verify independent replacement in staging.
- Activity → Saved has Browse and Current tabs within the existing 160-pixel
  panel. Current exposes save, rename, tag/clear tag, Markdown/HTML export and
  positive/negative last-turn ratings with optional notes. Browse includes
  Rebuild all titles; it runs Plank's global `/retitle`, not a per-row rename.
  Every shortcut prepares the composer for explicit Send. Current controls
  respect the runtime command catalog, and read-only observers cannot use them.
  Rename retains the old saved copy; exports are written on the engine host.
  Submitting `/export` opens Current. Once the complete file is received and
  verified, a single compact row shows its filename, size and Download action.
  Desktop clients choose a destination and stream into a temporary sibling,
  checking declared/received length and SHA-256 before replacing the destination.
  Failed or corrupt downloads preserve existing files and remove partials.
  Browser Download prepares an authenticated Blob without copying the complete
  file into WASM memory; Save file then runs from a fresh user click. Credentials
  stay in request headers. Suggested names are portable across desktop platforms.
  Downloads run independently of snapshot polling and agent controls. Browser
  object URLs are revoked on export/context replacement, stale completion,
  closed update channels and view disposal. Browser preparation retains at most
  one file and one in-flight fetch per view. Remote exports require host transfer
  support; older engines retain their host-local export behavior. Model-free
  tests cover atomic saves, corruption, length mismatch, HTTP failures and names.
  These actions retain Plank's existing command behavior. Once a saved-list page
  has been requested, successful saves (including persisted tags), deletes and
  title rebuilding republish that page automatically. Context resets/resumes
  republish after the reset frame, retaining the opened monitor. Removing the
  final row of a page clamps the remembered page to the current last page.
  Unopened panels and temporary sidechains do not trigger refresh scans. Rename
  still creates its new saved copy only at the next save. Refresh adds no command
  or transcript mutation; scan failures retain the previous list and report the
  failure without turning a successful mutation into a failed save. Model-free
  tests cover mutation updates, logical copies, reset order and page clamping.
  Embedded export previews and real interactive verification remain outstanding.
- Web searches and page/document downloads now poll the owning console channel's
  cancellation. Curl runs in a separate process group, drains bounded stdout and
  stderr buffers, and stops the group when cancelled. Native Chrome uses its C
  cancellation callback with an owned console reference; embedded-browser lock
  and navigation waits poll the same predicate. Page/search results and document
  conversions check cancellation before returning output. PDF conversion itself
  remains synchronous. Model-free process tests cover sleeping fetches, refusal
  before startup, and oversized pipe output. Native compilation and callback
  checks do not establish actual Chrome navigation. The embedded-browser build
  cannot finish here because the V8 archive is absent and its download fails DNS
  resolution; that transport still requires compilation and runtime verification.
- MCP requests now read the tool context's shared cancellation channel. Stdio
  writes use nonblocking pipes and a whole-exchange deadline; stdout waits poll
  in short intervals. Cancellation stops the server process group and retires
  the connection, preventing partial or late replies from leaking into another
  call. A later call can start a fresh handshake with the same cancellation
  binding. HTTP exchanges use isolated completion channels: cancellation drops
  the waiter, ignores late replies/session changes and quarantines the old
  connection; a later call may initialize a fresh session, without replaying the
  cancelled request. The abandoned HTTP exchange remains bounded by its existing
  timeout, and remote effects may already have occurred. Stdio process tests and
  HTTP completion-channel tests cover cancellation; actual HTTP/controller
  interaction and remaining blocking paths still require runtime verification.
- Synchronous tool, lifecycle and hook-backed worktree events now poll the
  owning input channel's cancellation while running shell hooks. Hook stdin is
  written on a separate thread, so a command that never reads its input cannot
  bypass cancellation or timeout handling. Cancellation kills the process group,
  skips later hooks and discards interrupted event context. Worktree hooks cannot
  report successful creation/removal after cancellation. Previously spawned
  asynchronous hooks retain their independent timeout behavior; cancellation
  prevents additional asynchronous hooks from starting. Web/MCP and other
  blocking operations, plus real host/controller verification, remain open.
- Foreground bash waits, requested `bash_status` waits and GPU-yield reruns now
  poll the agent's shared input-channel cancellation as well as local Ctrl-C.
  A stopped wait terminates the shell process group, retains the stop for the
  turn dispatcher and suppresses GPU unload/reopen triggered by a racing signal
  file. New tools are refused after cancellation, including calls intercepted
  for agent, fan-out and image handling; the dispatcher checks again after
  pre-tool checks. Process regressions stop sleeping shells, skip a subsequent
  file write and verify clean input after stop retirement. Other blocking paths
  (including hooks, web and MCP) and real controller interaction remain open.
- Quiet generation now carries the owning remote controller's interruption
  state, including memory prefill, extraction, serial sidechains and fan-out
  passes. Stop is checked before generation, by the engine stop hook and after
  the last callback, so late cancellation cannot be mistaken for a completed
  pass. Child passes retain the shared stop for their siblings; only dispatch
  cleanup retires it, including a coincident local interrupt. Cancelled memory
  jobs retain their queue position without spending a failure attempt, and
  stopped fan-out work cannot start another pending tool batch. Model-free
  regressions cover late cancellation, sticky stops, identified cancellations
  and clean retry. Blocking tools and real host/controller verification still
  remain before local GUI hosting can be enabled.
- Model → Engines shows the host's published and local catalog in 32-row pages,
  including versions, notes, installed/download/downloading/staged states and
  unavailable reasons. Build support, RAM, free download space and the current
  named engine accompany the report. `/engines --page N` uses the startup
  chooser's fit rules without loading a model or restarting. Unresolvable
  entries remain visible as disabled. Labels are bounded; oversized display
  names are abbreviated with row identifiers, while the chooser keeps complete
  names. Load and paging prepare commands for explicit Send. Choose opens the
  existing local chooser; remote requests are restricted to inspection until
  remote restart is implemented. Catalogs use the generation-fenced report
  adapter and independently coalesced replay. Older engines do not receive the
  new generated command unless their catalog advertises its syntax.
- Model → Metrics → Activity renders the engine's 53-week saved-session heatmap
  and eight headline figures. `/stats`, `/stats 7`, `/stats 30`, and `/stats all`
  publish the same calculations as the terminal report; changing the figures'
  scope preserves the annual grid. Cells use the terminal's four ranked
  intensities, and clicking one shows its host-local date and approximate
  tokens. Scope and refresh buttons prepare commands for explicit Send. The
  160-pixel panel scrolls vertically, and narrow windows scroll the calendar
  horizontally. Snapshots are requested, bounded to 371 cells, validated and
  independently coalesced in replay. Older engines retain text reports.
- Job rows include compact Output and Stop shortcuts, enabled only when the
  engine advertises `/jobs [output|stop <id>]`. Both prepare commands for explicit
  Send. The engine resolves tracked IDs and reads at most the final 32 KiB on its
  own host; missing jobs are refused rather than opening arbitrary client paths.
  Viewing or stopping does not consume the model's completion notification.
  Local Stop reclaims control; read-only output preserves the remote grant.
  Plain and TUI dispatchers share the implementation. Output remains a requested
  transcript report rather than a continuously streaming job terminal.
- Console generation callbacks now publish live prefill/generation status and
  job snapshots to an attached remote bus even when local GUI output is disabled.
  State transitions publish immediately; same-state updates and jobs coalesce
  to one second. Reconnect replay retains the latest status. Idle jobs, task
  changes after tool dispatch, and local prompt echoes also reach remote clients;
  status/tasks/jobs refresh at idle boundaries. Automatic background-job turns
  retire remote interruption after return, including early cancellation and
  errors; quiet memory passes clear it before the next input. This completes
  additional metadata wiring, not console-host runtime parity: interruption of
  blocking tools/quiet generation and remaining text/diff/notice output still
  need verification before hosting is enabled.
- Agent-owned profile trust and rename-overwrite confirmations now use the
  same shared console channel as other Yes/No dialogs. Only an explicit current
  reply grants consent; queued text remains input for the idle loop. The Yes/No
  order matches the existing GUI. Rename callbacks clone the channel rather than
  borrowing the mutable agent. Startup engine selection, which runs before an
  agent exists, reads exactly one bounded UTF-8 reply line and emits request
  completion on acceptance, decline, EOF, or read error. It cannot slurp later
  bytes into the standard library's stdin buffer. Plain unattended confirmations
  retain their auto-decline behavior. Real profile/plugin trust and startup flows
  still need end-to-end verification before enabling local GUI hosting.
- Network prompts and slash controls carry the controller and grant generation
  that accepted them. Ingress and dispatch use the same policy/queue lock order;
  releasing, regranting, reconnecting, or local takeover invalidates queued work
  from the previous grant. Rejected inputs are retired with their matching
  completion kind, so they cannot run after a later grant or interrupt another
  task. Legacy frames now enter the same bounded idle queue with internal IDs;
  legacy commands retain `command_done`, while identified clients retain their
  ID-matched completions. Explicit local prompts/mutations reclaim control and
  reject waiting network inputs; read-only reports preserve the grant. Buffered
  local stdin is checked before remote dispatch and before readiness. Incomplete
  local frames defer remote/background work. Hosts advertise `ownership_fencing`;
  native/browser submission refuses hosts missing this capability. The client/
  daemon protocol is 28. Local GUI hosting is still disabled pending remaining
  startup/trust/modal runtime verification, blocking/quiet-pass interruption,
  remaining text/diff/notice output parity, and real ownership/handoff verification.
- Console agent questions (including web approval), file/memory/profile editors,
  and profile-restart/memory-deletion confirmations use an owned inbox. Local
  and authenticated remote replies share one request identity and first accepted
  reply; explicit decline, cancellation, interruption, EOF, and reader failures
  close that identity on both streams. Typed local `dialog_reply` frames keep
  ordinary JSON prompts distinct from answers. Late replies stay out of model
  input; prompts arriving during dialogs survive for the idle dispatcher.
  Partial JSON and UTF-8 characters survive reader timeouts without replacement
  characters. The inbox bounds pending prompts to 64/4 MiB and JSON frames to
  2 MiB. Editor limits and source-change validation still apply. Readiness is
  deferred while preserved input remains. Module/pipe regressions cover competing
  replies, explicit Save/Cancel, invalid choices, EOF, Unicode, and cleanup;
  process fixtures check typed replies and another controller closing the local
  editor before the engine becomes ready. Local hosting remains disabled pending
  end-to-end ownership and startup/trust/modal runtime verification.
- Remote ingress permits the exact read-only `/rc status` and
  `/remote-control status` reports. Host toggles and self-grant commands remain
  local-only; adding arguments to the status report cannot bypass that guard.
- The console-host bridge's output layer mirrors parsed answer, reasoning,
  tool, and error events to an attached remote bus while retaining the primary
  terminal renderer. Transient preview rewriting stays local. Console slash
  reports also mirror their printed bytes without process-wide stdout
  redirection. This is groundwork for local GUI hosting; it does not enable
  `/rc` in console mode. The console idle loop now drains remote input snapshots
  through the plain dispatcher, keeping `/command` out of model prompts and
  completing identified inputs after their individual dispatch returns. Cancelled
  inputs are skipped and restricted host commands are refused again at dispatch.
  Errors are reported before completion without dropping subsequent queued inputs.
  Remote cancellation is checked before console turns and in the generation
  stop hook, including tool-stanza completion. The transient interrupt clears
  after dispatch so a stopped request cannot interrupt the next one. A
  `+PLANK_BUSY` marker tells the local daemon that another controller started
  work; the GUI stays running and rejects new prompts until readiness returns.
  Remaining startup/trust/modal runtime verification, blocking/background interruption, and
  end-to-end ownership verification still need connecting before hosting is
  available.
  Regression checks compare local output with and without mirroring and ensure
  read-only reports preserve the conversation.
- Remote slash controls now use `submit_command` and a unique request ID.
  Only the matching `command_request_done` completes the input; another
  controller's ID or a legacy `command_done` with identical command text is
  ignored. `/command` uses this path alongside the other controls. Rebuilt
  hosts advertise `command_request_completion`; the client refuses control
  submission without it. Commands share the bounded idle-dispatch queue and
  recent-ID window with prompts. Cancellation retains completion kind and
  retires acknowledged requests so the dispatcher cannot revive them.
  Legacy wire messages remain supported for older clients. Actual host/modal
  and control-handoff interaction still require runtime verification.
- Ordinary remote prompts now use `submit_prompt` with a unique request ID;
  only its matching `prompt_done` releases the client input. Shared desktop
  notifications and other clients' completions cannot finish that input.
  Rebuilt hosts advertise `prompt_completion`; older hosts are refused for
  ordinary prompts. Identified
  prompts have a separate 64-item idle-dispatch queue, so a busy host finishes
  its prior turn before starting their batch. The dispatcher acknowledges only
  IDs taken for that batch after its turn returns. Later arrivals remain for
  the next turn. A 128-ID recent window rejects duplicate submissions; IDs,
  text size, and queue bounds are validated. Legacy prompt behavior remains
  available to older clients. Model-free transport tests check that unrelated
  notifications/completions leave the submitting client running. Stop uses
  `cancel_prompt` with that ID: queued inputs are removed without interrupting
  the current host turn, dispatched cancellations are skipped, and running
  cancellation survives the busy UI's transient interrupt reset. Finishing
  retires the ID so late cancellation cannot interrupt the next task.
- Model → Remote can attach as a read-only observer without submitting a
  prompt, then explicitly request or release control. Observer authentication
  uses `observe_auth`; a rebuilt host never acquires or reclaims a controller
  slot for this message, and older hosts refuse it. Normal first-input
  connections now request control instead of rejecting a read-only greeting.
  One submitted input stays on the client until a grant arrives; duplicate
  grant frames cannot resend it. Releasing retains the socket and live output.
  Attachment role, client ID, denial reason, and waiting-input state remain in
  generation-fenced daemon memory. Observer question/editor actions are disabled
  and rejected by the daemon. Stop while waiting closes without interrupting
  the host; grants arriving after generation invalidation release the slot
  without sending the held input. Transport regressions use real WebSocket
  frames over Tokio duplex streams. TCP/TLS handoffs remain unverified, and
  actual host dispatcher completion still needs end-to-end runtime verification;
  slash commands use their matching completion frames.
- Activity's Model → Remote view displays an existing Plank host's loopback
  endpoint, controller, automatic-grant policy, reconnect reservation, and up
  to 64 waiting client IDs with a total count. `/rc status` is a new read-only
  report command; Grant next and per-client Grant prepare `/grant` or
  `/grant ID` for explicit Send. Granting hands away control and clears the
  other waiting requests. Changed host
  policy snapshots arrive once per second even while the engine is busy or
  idle; snapshots omit the bearer token, are validated and generation-fenced,
  and replace earlier policy snapshots in replay. The address belongs to the
  engine host and needs tunneling for another machine. A running host retains
  its original grant policy until stopped and started again. Hosting actions
  remain unavailable for local GUI sessions: the console loop still needs
  ownership and startup/trust/modal runtime verification. Actual TCP/grant handoffs remain
  outstanding. Metadata and `/rc status` require a
  rebuilt Plank engine.
- Activity's Model → Metrics view contains Context, Usage, and Speed tabs in
  the same 160-pixel scrolling panel. `/context`, `/usage`, and `/toks` populate
  typed snapshots on local and remote engines and select the matching view.
  Load/refresh and Compact prepare commands for explicit Send. Context shows
  resident fill separately from raw tokenizer estimates for system, MCP,
  messages, AGENTS.md, and memory; estimates are not presented as an exact
  partition of resident tokens. Usage reports numeric session input/output,
  provider cache counts and pass counts when available, and elapsed time.
  Input includes cached provider tokens; local input uses Plank's context-growth
  estimate. Speed charts retain up to 64 process-wide decode-window and
  completed-prefill samples, with independent scales and chronological ordering.
  These are requested snapshots, not continuously refreshed charts. Payloads
  are bounded, validated, generation-fenced, cleared on context replacement,
  and independently coalesced by kind in replay. Older engines keep text
  reports. Live measurement refresh remains outstanding.
- Activity's Model → Downloads view shows engine setup and the detached model
  update helper in the same bounded panel. Download/resume, Report, Stop keeping
  partials, and Stop deleting partials prepare existing `/model` commands for
  explicit Send. The helper reports the current artifact, transfer progress,
  rate, checksum verification, cancellation request, and last terminal result.
  Progress is per artifact rather than an estimate for the whole model set.
  Local and remote hosts publish changed snapshots once per second independently
  of generation. Completed, failed, and cancelled results remain visible after
  the helper exits; dead/stale active states are discarded. Snapshots are
  validated, generation-fenced, and coalesced in replay. Verified artifacts
  survive cancellation; keeping partials allows later resumption. Older engines
  retain text reports. Remote engine restarts remain
  outstanding.
- Activity's Cache tab shows engine-host KV lineage in 32-row pages, with role,
  name, model/session details, size, hits, age, pin/expiration markers, and the
  engine's total/reclaimable storage summary. Load/refresh and paging prepare
  `/kvcache --page N`; Pin/Unpin, Remove, Clean up, and Strip saved payload
  prepare commands for explicit Send. Modern engines supply stable opaque file
  handles paired with full fingerprints, so files sharing a payload fingerprint
  remain independently actionable. Handles come from the same scan as the
  lineage; mutations resolve the handle again and check current sidecar identity
  for pin, unpin, and removal. Missing/replaced entries are refused. Older
  snapshots without handles retain fingerprint commands and read-only duplicate
  rows. Synthetic group headings are read-only. Cache bodies
  stay on the engine host. Local/remote snapshots are requested, bounded,
  generation-fenced, cleared on context reset, and coalesced in replay; typed
  mutations refresh the inventory and terminal-pane mutations also publish
  updates. Bare `/kvcache` keeps its legacy tree output/pane. Older engines
  retain bare text reports. The shared native/browser panel stays within 160 px.
- Activity's Extensions tab contains Plugins, Skills, Templates, Hooks, MCP,
  and Agents inventories. Load/refresh prepares the category's existing slash
  command for explicit Send. Engine-authored reports include loaded plugin
  origins/contributions and held components, skill/template/agent descriptions,
  hook event matchers, and MCP server status with advertised tool names and
  descriptions. Skills and hooks report their actual session switches.
  Management buttons prepare install/remove/info/trust/enable/disable, publisher,
  Claude plugin and profile commands; installation/removal and disable changes
  retain Plank's next-start semantics. Loaded extension commands have direct
  prepare shortcuts. Inventories are requested rather than polled, bounded to
  64 KiB plus a truncation notice, cleared on context/worker replacement, and
  independently coalesced by category for remote replay. Older engines keep
  their transcript reports. The shared native/browser panel stays within 160 px.
- Runtime command catalogs include capability requirements. The picker displays
  these below affected command descriptions and includes them in search, while
  keeping commands selectable for status, disabling, and partial functionality.
  Session details show engine-reported build and loaded-model capabilities with
  explanations. The default view is one collapsed availability-count button;
  expanding it reveals the features in a 160-pixel scrolling area. The catalog
  reports 17 capabilities, including attachment support separately from model
  vision, structured conversations, asides, interleaved generation, decisions,
  GPU release/restore, exact KV truncation, backend concurrency and local inference.
  GPU restore requires an actual reopen recipe; concurrency does not promise
  that configured agent budgets or other subagent backends permit parallel work.
  Model-free tests verify that availability follows the loaded backend without
  generation or transcript changes. Older engines omit this information instead
  of guessing support. Real native/browser disclosure interaction remains to verify.
- Activity's Branches tab shows Checkpoints and Fork points in 32-row pages.
  Load prepares `/checkpoint --page N` or `/tree --page N`; checkpoint names,
  captured-message summaries, numbered user prompts, and a bounded tree preview
  come from the engine; Tree toggles the preview without expanding the default
  panel. Roll back, Fork here, New checkpoint, and Clone branch
  prepare commands for explicit Send. Checkpoints restore the conversation,
  task list, off-path branches, and optional engine cache; they remain in memory
  for the current session. They do not restore project files. Rollback retains
  the discarded conversation as `pre-rollback`. Local and remote resets clear
  live metadata and append a durable context-boundary note. Separate checkpoint
  and fork snapshots are generation-fenced and independently coalesced in replay.
  Once opened, each category remembers its own page. Checkpoint creation,
  rollback, fork and clone preserve that page and clamp it after a shorter history.
  Idle boundaries refresh opened categories so normal prompts appear as fork
  points without another `/tree` command. Clear/resume reset frames precede
  refreshed snapshots, after old checkpoints have been cleared. Unopened idle
  monitors and temporary sidechains do not generate history snapshots; refresh
  adds no prompt or transcript mutation. Model-free regressions cover independent
  pages, rollback clamping, new prompts, reset ordering and sidechain isolation.
  Actual native/browser interaction remains to verify.
  Engine command descriptions now match the actual conversation semantics.
- Activity's Settings tab edits engine-host settings in 32-field pages,
  including plugin-declared options. `/config` and `/config --page N` publish
  authoritative labels, values, and accepted-value hints; setters refresh the
  page containing the changed key. Prepare copies a single-line setter into the
  composer, and explicit Send uses Plank's existing validation and persistence.
  Empty optional values clear settings; tri-state defaults retain setter syntax.
  Larger values are displayed read-only rather than saving clipped content.
  Pages are validated, kept in daemon memory, generation-fenced, and coalesced
  in remote replay. Native and browser clients share the compact scrolling form.
  Older engines retain `/config` text output and direct setters.
- Activity's Saved tab browses engine-host conversations separately from workspace
  tabs. Load/refresh and page arrows prepare `/list [page]`; each metadata page
  contains at most 64 sessions with bounded title, tag, and directory text.
  Full validated IDs prepare Resume and Delete commands; Search and Save current
  use the same explicit submission flow. Native and browser clients share the
  panel and local/remote wire adapter. Lists are fetched only on request, retained
  in memory, and fenced by worker generation. Bare `/list` preserves the full text
  report; specifying a page bounds both its report and metadata. Remote replay
  coalesces saved-session snapshots. Older engines retain the text-report fallback.
- Activity includes a compact Model tab with reasoning, temperature, GPU power,
  speculative decoding, micro-compaction, and loop-guard presets, plus custom
  values. Engine selection, download status, steering, context, usage, and
  compaction are accessible there. Every action prepares the composer and
  requires explicit Send. Rebuilt engines include model, temperature, configured
  power limit (zero means default), speculative decoding and its availability,
  micro-compaction, loop guard, and optional steering scale in control catalogs.
  The panel displays these authoritative readings; older catalogs show unavailable
  values. The unsupported speculative-decoding preset is disabled. Catalogs are
  validated and cleared on engine replacement/context reset; remote replay retains
  only the latest catalog. Capability explanations are available in session details.
  Catalog availability is available in Model → Engines. Controls wrap
  and scroll within the same 160-pixel panel on native and browser clients.
- The Activity toggle opens a bounded panel with Tasks & goal and Jobs tabs.
  Job snapshots show IDs, PIDs, elapsed time, state, exit code, and engine-host
  output paths. Running jobs come first; snapshots cap rows at 64 while retaining
  full running/total counts. Report prepares `/jobs` for explicit submission.
  Local idle polling and generation callbacks refresh jobs once per second;
  remote hosts publish at idle and tool boundaries. The remote adapter retains
  its authenticated controller connection between inputs, so idle updates keep
  reaching the client without submitting another prompt.
- Remote input completion returns the session to idle while preserving the
  socket. Further prompts and slash commands reuse its control grant; each slash
  command still requires its matching completion frame. Disconnection requires
  recovery instead of automatically replaying an uncertain input. Tests exercise
  authentication, idle telemetry, successive commands, and orderly closure with
  WebSocket frames over a duplex stream; TCP/TLS verification remains outstanding.
- The Tasks & goal tab opens a bounded, scrollable report without a
  permanent panel. Local tool batches and goal iterations publish fresh reports;
  remote frames carry the same engine-authored report. Set goal, Goal status,
  Clear goal, and Report prepare slash commands for explicit submission.
  Older remote servers fall back to requesting `/tasks`. Reports are capped at
  256 KiB, with a truncation notice directing users to the complete `/tasks` output.
- `/command` is a discoverable alias for `/commands` in both dispatchers.

- Desktop and browser session views display a compact live status line with
  context tokens, generation throughput, reasoning level, and task counts.
  Remote status/task frames and local console telemetry share one adapter.
  Local generation updates are coalesced to one second; state transitions
  publish immediately, and idle refreshes include current context and tasks.
  Counters stay in daemon memory rather than SQLite, and replaced workers
  cannot publish into a new generation. Interrupted/recovery sessions hide
  stale readings. Local engines must be rebuilt to emit telemetry.

- Local and rebuilt remote engines publish built-in and loaded extension catalogs. The picker uses
  these at runtime, with the bundled catalog as a startup fallback. Engine
  replacement and lost context invalidate the runtime catalog.
- Arrow keys select commands, Tab or Enter prepares them, and Escape dismisses
  the picker. Selection never executes commands. Ctrl-Shift-P opens it outside macOS.
- The bundled Plank console loop dispatches slash commands through its shared
  plain-REPL dispatcher instead of forwarding them to the model.
- Commands use the existing durable prompt submission and deduplication path.
- Compact buttons and spacing avoid a permanent toolbar for each feature.
- Local stderr control frames retain partially read bytes when stdout or user
  replies arrive. A process regression fragments catalogs across both streams
  so restart readiness cannot hide a lost catalog.
- Local Plank questions and yes/no confirmations use structured stderr events
  and matching JSON answers. The UI supports single and multiple selections,
  explicit decline, and rejection of outdated requirement revisions.

- `/open`, `/memory`, and `/edit-profile` open paged line editors. Drafts persist;
  Save and Cancel are explicit. Engine validation rejects files or profiles
  changed during editing. Editors currently limit text to 256 KiB.
- Editors have Undo/Redo buttons, Cmd/Ctrl-Z and Shift-Z shortcuts, and direct
  line-number navigation. Consecutive typing is grouped; pasted and removed
  lines retain their identities through undo. Draft history survives recovery
  and is bounded to 128 changes and 4 MiB per document.
- Find opens a compact literal, case-sensitive search row; Cmd-F on macOS or
  Ctrl-F elsewhere focuses it. Search covers the full loaded draft, jumps pages,
  highlights the matching line, and shows match position/count. Enter/Shift-Enter
  and arrow buttons navigate with wrapping. Queries stay out of persisted drafts;
  edits and history navigation invalidate stale match positions. Explicit search
  navigation selects the matching text within its first line; F3/Shift-F3 and
  Cmd/Ctrl-G cycle matches after focus moves into a line. Go to line focuses its
  initial caret. The locally ejected toolkit Input keeps stable navigation tokens
  and prunes cached entities, focus subscriptions and text origins outside the
  visible page. Find includes a compact replacement field and Replace action:
  replacement handles matches across lines/pages as one undoable change, preserves
  unaffected line identities, and advances to the next match. Search/replacement
  values stay out of persisted drafts. Saving remains explicit. Cross-line mouse
  selection, larger-file parity and real native/browser focus interaction remain
  incomplete.
- Remote controls use a separate queue, wait for the idle dispatcher, and return
  matching completion frames. Reports and static acknowledgments cross the
  wire. Bare `/config` returns settings instead of opening a local form.
  Slash controls require a rebuilt remote engine advertising this capability.
- Host TUI question panels publish structured choices to remote clients. Only
  the authenticated controller can answer the current question; single/multiple
  choices, explicit decline, and “Chat about this” retain Plank's semantics.
  Local answers and interruptions close the remote form. Invalid or late
  answers produce a rejection without cancelling the turn. Single-choice
  command panels also use this bridge: rename overwrite, memory deletion,
  buddy selection, startup profile trust, and the startup AGENTS.md offer.
  Local option ordering is preserved, including safe first choices for
  destructive confirmations. Accepted remote replies are reconciled with local
  Enter/Escape; Ctrl-C and remote interruption cancel, and every exit closes the
  request. Model-free tests cover option mapping, explicit decline, invalid and
  late replies, and interruption. Real host/client interaction remains to verify.
- Remote `/open` text files, `/memory`, and `/edit-profile` use the same compact
  editor as local sessions. Replies require the current controller and explicit
  text or cancellation; host Escape/Ctrl-C also cancels. Source changes are
  checked before saving. Remote profile edits apply on the next engine restart;
  remote restart and remote HTML previews remain incomplete.


- Local `/engines` restarts into a paged engine chooser without a terminal.
  Downloads show transient progress and offer leave/cancel controls. Cancelling
  selection preserves the current CLI engine. Saved conversations resume under
  a matching engine family; other families keep the old saved session intact.
  Resolved launch arguments persist so Fresh context keeps the chosen model.
- Saved profile edits offer a restart. Headless startup restores the requested
  session before warming and firing SessionStart. Questions before readiness
  defer the first prompt instead of consuming it as an answer. Download questions
  close automatically on completion; late answers cannot act on a finished request.
- Startup profile trust offers use the same headless question bridge and require
  an explicit answer before changing trust.

The engine fix requires rebuilding Plank (`just build-plank`) and selecting
that executable when creating a local session. An older installed `plank`
binary does not gain command dispatch from updating the GUI alone.
The client/daemon protocol is now version 28, including delegated-run snapshots, live progress, rendered tails and reports, paged engine inventory, saved-session activity heatmaps, required remote ownership fencing,
typed local dialog replies,
remote-driven console busy transitions,
identified slash-control and remote-prompt completion,
observer attachment and role management,
credential-free remote-host policy,
context/usage/throughput measurements,
live model-download snapshots,
stable cache file handles and lineage pages,
requested extension inventories, paged configuration and
saved-session metadata alongside remote document replies, startup-question
deferral, and engine launch/progress handling. Finish active runs before restarting
an older daemon; a client detects the mismatch instead of issuing unsupported
commands.

## Remaining requirements

- Complete-file export transport now has transient begin/chunk/end events for
  console and remote adapters. Each UTF-8 chunk is at most 32 KiB, with contiguous
  byte offsets, expected total length and a SHA-256 digest. Empty files complete;
  failed consumers stop emission without a completion event. Suggested names
  omit host paths and unsafe basenames; debug output excludes file content.
  Transfer events never enter transcript rendering or reconnect scrollback.
  Model-free tests verify large Unicode files, worst-case JSON escaping, checksum
  metadata, live delivery and wire roundtrips. Emission remains internal opt-in
  (`PLANK_GUI_EXPORT_STREAM=1`), enabled for local daemon workers whose pipe provides
  backpressure. Remote clients negotiate `export_stream` on their auth frame;
  legacy subscribers receive no file bodies. Export-capable connections use an
  eight-event queue separate from ordinary output, with ordered merging by
  sequence ID. A stalled reader retires only its current export after two seconds;
  other readers continue, ordinary output remains live, and cancellation retires
  partial transfers. Malformed/oversized events are refused before enqueueing.
  The daemon stages chunks in anonymous files without accumulating
  complete bodies in memory or SQLite. It validates identities, offsets, declared
  lengths and checksums before publishing the latest export descriptor. Reset,
  worker generation changes and failed/incomplete transfers discard staged files;
  worker shutdown discards partials while retaining an already verified export.
  Authenticated `/v1/export` streams a verified file as an attachment, with
  generation and opaque artifact ID checks, independent reader offsets, no-store
  and nosniff headers. Receivers never interpret a filename as a host path.
  Model-free tests cover staging, corruption, stale cleanup, API authentication,
  exact downloaded bytes, reset invalidation and bounded multi-reader delivery.
  Actual host/client transport, slow-socket eviction and
  real native/browser retrieval verification and embedded previews remain outstanding.
  Existing `/export` still writes the complete file on the host.

- Bridge all remaining permission and trust prompts; verify actual engine
  question emission and completion end to end, beyond the adapter fixture.
- Local console remote-control hosting, remote-turn runtime verification,
  real grant-handoff verification, and embedded WASM frame rendering.
- Console hosting must verify local/remote arbitration, complete startup/trust
  and remaining modal paths, and verify shared questions/editors with real
  controllers beyond model-free fixtures.
- Cross-line editor selection, larger-file parity, and real focus/navigation verification.
- Verify runtime capability reporting with actual loaded engines, including
  vision, GPU reopen recipes and backend concurrency.
- Live metrics runtime verification, full insights chart/HTML parity, richer task/goal and job interaction,
  richer branch/checkpoint and saved-session management, per-component extension
  forms and restart management,
  broader model/feature availability, and per-artifact download inventory.
- Remote profile/engine restart, HTML previews, and remaining modal question paths
  must match local behavior.
- Verify command results with the real engine, interactive response lifecycle,
  crash/reconnect behavior, native UI flows, and browser UI flows.

## Verification

`just check-plank-controls` runs focused model-free console, catalog, editor,
question, and remote-control regressions. Client tests check catalog validation,
generation fencing, explicit edits and answers, and remote completion matching.
Full control coverage remains incomplete until the requirements above are
implemented and verified.
