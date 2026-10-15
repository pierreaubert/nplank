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

- Document undo now stores checked text-range changes for large row spans instead
  of retaining complete before/after bodies. Compressed identity runs restore
  stable row IDs across splits, joins and recovery without one UUID per row.
  Typing and IME groups compose into one inverse. Every application validates
  expected identities, text length, UTF-8 byte boundaries and both revisions'
  SHA-256 before changing the document. Legacy row-based journals remain readable.
  Tests cover a small edit in a 32 MiB line, 10,000-line splits/joins in an 8 MiB
  row, recovered undo/redo, composition, malformed identity counts and stale or
  corrupt history. The combined 128-change/4 MiB history budget still applies to
  actual changed text and metadata; large destructive edits can exceed it.

- Long native input rows now render a caret-following window of at most 256
  Unicode scalars instead of shaping the entire line. Ellipses indicate hidden
  text; the model and clipboard retain the full value. Ordinary grapheme clusters
  stay intact at viewport edges, while pathological oversized clusters keep a
  bounded excerpt. Hit testing and IME bounds translate between the window and
  full-line scalar/UTF-16 positions. Native undo/redo shares a 4 MiB text budget
  and retains one oversized inverse, preventing 200 full copies of a large paste.
  Regressions cover a 32 MiB Unicode line, grapheme boundaries, coordinate mapping,
  complete selection copying and repeated oversized undo/redo. Physical font,
  pointer and IME interaction remains unverified. Cursor lookup and model edits
  still have linear work on long lines; streamed source opening and shared remote
  original bodies are implemented, but physical large-file interaction remains
  unverified.

- Editor originals now live in separate database chunks; polling session metadata
  carries only byte length and SHA-256. An authenticated, revision-scoped read route
  serves at most 256 KiB per response. Native and browser clients reassemble byte
  chunks, validate length, checksum and UTF-8, and publish only a complete draft.
  One background original load per view leaves polling and commands responsive.
  Loading and failed states offer Cancel and Retry without replacing recovered
  drafts. Late results for closed/replaced editors are discarded. Schema 3
  transactionally migrates legacy inline originals and cleans retired/orphaned
  chunks. Tests cover cross-chunk UTF-8, authorization, stale reads, bounded
  snapshots, migration rollback, recovery precedence, malformed transfers and
  cleanup. Protocol 44 requires matching client and daemon builds. Streamed existing-file
  opening now accepts up to 32 MiB (legacy inline opening remains limited to
  256 KiB); this change removes repeated bodies from the snapshot path.

- Remote document opening now negotiates `editor_stream` during authentication.
  Opted-in clients receive bounded begin/chunk/end requests with checksummed text;
  legacy clients retain inline editors. Reconnect replay sends individually framed
  entries and ends with a snapshot marker, which the daemon waits for before
  submitting input. Partial documents remain invisible and are discarded on editor
  closure, replacement, reset or disconnect. Replay notifications remain suppressed.
  Snapshot capture and live subscription now share one bus lock, preventing events
  emitted during replay from being lost or duplicated. Wire tests cover legacy,
  chunked and replay framing; client tests cover completion, closure, malformed
  nesting and real duplex opening/Save sequencing. Negotiated streaming opening
  accepts existing files up to 32 MiB; legacy clients retain the 256 KiB cap.

- Saves now accept edited drafts up to 32 MiB when the current engine advertises
  `editor_stream_save`. Older engines retain their 256 KiB inline limit; the
  upload route rejects unsupported sizes before polling the body and execution
  rechecks the current capability. The editor disables Save above the negotiated
  limit, shows that limit and retains the draft and Cancel action. Byte counting
  walks shared chunks without allocating a whole document each render. A real
  HTTP/process regression uploads exactly 32 MiB of Unicode/escaped text and
  verifies the saved bytes and matching acknowledgment. Existing-file opening
  accepts up to 32 MiB when streaming is negotiated, otherwise 256 KiB.

- Browser drafts and editor undo history now use IndexedDB instead of sharing
  localStorage's small quota with appearance settings. Writes are serialized,
  coalesced and acknowledged only after transaction completion. Failed snapshots
  remain queued; a separate compact error row offers Retry save and survives
  unrelated command successes. Legacy preferences migrate before their large
  localStorage entry is retired. Appearance-only windows update a separate record;
  native appearance writes lock and merge the latest file rather than replacing
  its drafts. Queue and native persistence regressions pass, and WASM compiles.
  Actual browser commit/reload and failure interaction still need verification.
  Concurrent native windows merge preference saves per key against each
  window's baseline, so distinct drafts survive and deletions are preserved
  (same-key conflicts resolve to the later saver); browser windows still use
  last-write-wins IndexedDB snapshots.

- Editor Saves now use authenticated UTF-8 uploads instead of embedding file
  text in JSON commands. This fixes valid edits failing at the general 128 KiB
  command-body limit, particularly when quoting/control characters expand in
  JSON. The upload has a negotiated bound, declared length and SHA-256 checksum;
  incomplete, corrupt and invalid UTF-8 bodies never reach the engine. Exact
  requirements and controller access are checked before reading and again
  before enqueueing, so a reset during upload cannot submit to a replacement
  editor. Empty text saves an empty file; Cancel retains its distinct command.
  HTTP/process tests cover the full limit, pre-read rejection, changed revisions,
  corruption and empty Saves. Protocol 43 requires matching client/daemon builds.

- Local document opening now uses bounded, checksummed chunks when the daemon
  opts in with `PLANK_GUI_EDITOR_STREAM=1`; small documents and legacy clients
  retain inline requests. The daemon publishes a form only after validating the
  entire text, and discards partial openings on closure, context reset, engine
  restart, readiness or replacement by another dialog. Late completions cannot
  revive closed forms. Frame bounds include escaped metadata. Parser, real
  process and isolated engine tests cover Unicode, opt-in compatibility,
  premature completion, cancellation and subsequent explicit Save. Streamed
  opening accepts up to 32 MiB; legacy inline opening remains limited to 256 KiB.

- Editor drafts now store shared text chunks rather than an allocated record
  and UUID for every line. Visible rows derive stable identities on demand;
  page iteration skips hidden rows directly. Splices, undo/redo and compact
  recovery retain untouched line identities. Legacy row-array drafts and undo
  records remain readable. Select-all copying uses chunk text instead of
  allocating one temporary string per row. The 32 MiB all-newline regression
  retains less than 1 MiB of buffer metadata beyond its text and verifies last-page
  navigation, selection and small-edit undo. Mixed splices, serialization,
  legacy recovery and native input regressions cover the new representation.
  Streamed existing-file opening uses the larger-file transport; snapshots carry
  only document metadata.

- Local and remote editor Saves now negotiate `editor_stream_save`. Drafts larger than
  one 32 KiB chunk use ordered begin/chunk/end frames with total byte length
  and SHA-256 validation. The worker returns to its multiplexed event loop
  between frames; replaced approvals, lost control and explicit rejections stop
  the pending sender. Local writes use a bounded frame queue with cancellable
  partial writes, so unread stdin cannot prevent stderr/output processing.
  Streamed local Saves retain their form until the engine's matching editor
  acknowledgment. Console and remote bytes cannot share an attempt.
  The host binds remote partial transfers to the exact controller
  grant, including same-client regrants; Cancel, close and editor replacement
  discard partial data. Old hosts retain inline Saves. A shared receiver accepts
  up to 32 MiB, as does negotiated client Save submission. Streamed existing-file
  opening also accepts up to 32 MiB; legacy inline opening retains 256 KiB. In-memory
  WebSocket tests verify negotiated and legacy delivery, escaped Unicode text,
  interleaved host output and separate command completion. Process tests verify
  local acknowledgments and editor closure while stdin remains unread; console
  tests verify fragmented Unicode, preserved prompts and EOF without implicit
  saving. A real pipe test transfers a document larger than one 4 MiB read batch;
  batches stop at the bound without consuming subsequent bytes, and incomplete
  UTF-8 retains at most three bytes between reads. Physical native/browser
  interaction remains to verify.

- Exact `/notify <mode>` now uses independent input during local/remote turns
  when `live_notify` is advertised. Existing Session buttons highlight the
  reported policy. Always, Unfocused, Never and legacy explicit on/off values
  preserve their meaning; bare toggles retain ordinary dispatch. Local GUI
  engines now delegate completion notices to clients when the daemon opts in
  with `PLANK_GUI_NOTIFICATIONS=1`, enabling desktop/browser delivery through
  the same bounded feed as remote notices. Legacy clients retain terminal
  delivery. Malformed local notices are ignored and cannot complete or kill
  a turn. Tests cover live generation, grant/capability checks, wire compatibility,
  opt-in emission and local pipe delivery without changing transcripts. Real
  OS/browser banners and native mobile delivery remain unverified or incomplete.

- Exact `/command` and `/commands` submissions now open the graphical command
  picker directly instead of merely printing the catalog count. Idle, writable
  sessions still dispatch the alias to refresh the authoritative engine catalog.
  During active turns, frames or observer attachments, opening the picker is
  local inspection and sends no engine request. Catalog reload is disabled for
  observers. Exact aliases bypass completion acceptance so Enter opens discovery;
  custom command names and argument-bearing input retain ordinary dispatch.
  Parser and engine-dispatch regressions cover the aliases; physical picker
  focus and interaction remain to verify.

- Live transcripts now collapse loaded entries before the latest context reset
  into one expandable Earlier context row. The active context has a compact
  separator; requesting older pages shows their records directly. New daemon
  boundaries use a distinct entry role, while existing system markers remain
  recognized. Stream chunks never merge across a boundary, and hidden reference
  history is not copied into rendered messages. All records remain stored.
  Tests cover legacy/typed markers, Unicode chunk coalescing, duplicate remote
  resets, persisted boundaries and retrieval of earlier pages. Physical
  native/browser transcript interaction remains to verify.

- Successful `/fork` and `/rollback` now publish a conversation-reset boundary
  before replaying retained history to graphical and remote clients. The shared
  reset path retires stale dialogs, artifacts and auxiliary views; the engine
  republishes its task state. Rewinds preserve checkpoints and off-path branches,
  clear the old suggestion and recheck hints at the next turn boundary. Read-only
  `/fork`, invalid targets and `/clone` do not emit resets. The tree renderer now
  ends active runs at a rewound cursor instead of marking the discarded tail
  active, including empty branches after root forks. Process, replay, branch and
  checkpoint tests cover these paths; loaded-engine interaction remains to verify.

- Insights now includes deterministic chart data separately from model-authored
  prose: tools, languages, projects, failure categories/tools/rates, reply times,
  host-local prompt hours and daily satisfaction. Native and browser clients
  share a single-chart view with previous/next navigation and a Text report
  toggle, within the existing Metrics panel. Counts use independent scales;
  percentages use 0–100%. Timing coverage and truncated category lists are
  explicit. Labels and row counts are bounded; malformed measurements and
  duplicate chart kinds are refused before display. Old hosts fall back to the
  text report. Tests cover units, Unicode bounds, ordering, coverage, legacy
  wire compatibility and daemon ingestion. Full HTML layout and physical
  native/browser rendering remain to verify or implement.

- Exact `/hints on|off` commands now use independent setting input in local and
  remote sessions that advertise `live_hints` in addition to their transport's
  live-control capability. The preference is recorded during generation; its
  model-facing instruction still lands at the next turn boundary. Settings →
  Session highlights the reported preference using the existing buttons.
  Older hosts retain ordinary idle dispatch. Generation-process and authenticated
  Unix WebSocket tests verify application without interrupting or completing
  the active turn; daemon/recovery tests cover capability gating, retry and
  deduplication. Physical loaded-engine interaction remains to verify.

- Startup offers for missing main models and MTP/vision companions now use the
  shared graphical confirmation when stdin is a pipe. The offer identifies the
  file, source URL, size and partial-download policy, then hands accepted
  transfers to the existing cancellable graphical downloader. Startup model
  update offers also reach the GUI, with version, total size and release notes.
  Only an explicit matching Yes accepts; terminal defaults remain unchanged.
  Isolated subprocess tests cover main/companion refusal, accepted/declined
  updates, stale replies and retaining subsequent prompt bytes. Live network
  downloads and loaded-engine startup still need interactive verification.

- Remote completion notifications now carry the host's `/notify` mode to the
  client. Desktop delivery runs on a bounded background worker; browser delivery
  requires permission from Settings → Session. `unfocused` checks the client's
  selected session and window visibility. Initial snapshots seed deduplication
  without banners, replay entries are recorded without delivery, and live events
  are accepted once per host instance. Notifications expire after 30 seconds;
  snapshot timestamps allow different client/server clocks. Context replacement,
  host replacement and stale generations retire pending notices. Notifications
  do not complete prompts, change attention or enter transcripts. Tests cover
  policy, replay, duplicate resets, host changes, bounds and clock differences.
  A WebSocket fixture verifies that duplicate notices during an identified main
  prompt deliver once and leave it running until its matching completion.
  Older hosts without a notification mode are ignored. Actual OS/browser banner
  delivery and permission interaction remain to verify; native Android/iOS
  notification delivery is not implemented.

- Document-native edits now carry explicit replacement ranges rather than
  inferring insertion text from string differences. When a selection spans
  rows, the surviving row receives the canonical text, selected range and marked
  composition. The old input is rebased immediately, so additional IME updates
  before repaint cannot duplicate preedit or target a deleted row. Navigation
  transfers the mark to the surviving input, and the retired input's blur cannot
  split document undo history. Model/widget tests cover forward/reverse
  selections, repeated pre-handoff updates, commit, original row identity
  restoration, multiline paste, unmark, alias retirement and transient-state
  serialization. Platform focus handoff and real IME interaction remain to verify.

- Native editor IME callbacks now retain a transient marked-text range. Repeated
  preedit updates replace the previous composition instead of inserting copies;
  explicit replacement ranges take precedence. Composition-relative UTF-16
  selections are translated after insertion, including after emoji prefixes.
  Commit/unmark removes the mark, preedit is underlined using the measured text
  layout, and the editor defers ordinary keyboard actions during composition.
  One composition shares a native-input undo entry. Navigation, external text
  replacement and focus cleanup retire stale marks; matching parent updates
  preserve them. Tests cover updates, commit, empty preedit, UTF-16 replacement,
  relative selections, undo/redo and cleanup. Composition across document rows
  now uses the explicit handoff above; real platform dispatch still needs verification.

- Editor pointer hits and native IME bounds now use measured glyph positions
  from GPUI's text shaper instead of a fixed eight-pixel column. Measurement
  follows the renderer's separate before/selected/after text pieces and its
  cursor gap, preserving the current appearance. Pointer hits choose glyph
  cluster boundaries; platform point queries convert scalar positions to the
  required UTF-16 offsets. The per-input cache retains coordinates rather than
  document or password text, and glyph mapping avoids repeated full-line scans.
  Tests cover proportional/scaled geometry, cursor gaps, Unicode clusters,
  UTF-8-to-scalar mapping, UTF-16 results and empty lines. Real platform font,
  display-scale and IME behavior remain to verify physically.

- Document inputs now share a transient pointer gesture within the current
  editor. Ordinary dragging can extend a selection into another row without
  Shift. Moving onto an existing previous/next-page button turns one page per
  visit, retaining the anchor; returning to a row rearms page hover. Release
  anywhere in the window or a keyboard action retires the gesture. Painted text
  layout replaces guessed offsets and fixed-width columns. Model-free tests cover scope
  isolation, release, page-hover latching, Unicode cut and undo. Native/browser
  event routing, scrolling, font-scale accuracy and IME interaction still need
  physical verification; no additional editor chrome is introduced.

- Local graphical sessions now submit explicit `/mc on|off`, `/loopguard on|off`
  and `/lg on|off` independently of the main prompt when `console_live_controls`
  is advertised. Existing Model settings rows prepare these commands for Send;
  no additional panel is needed. The console applies them during input polling
  and publishes updated settings without interrupting generation, submitting a
  model prompt or completing the active turn. Receipts deduplicate retries;
  queue refusal leaves the request retryable. Process tests exercise a real
  headless generation stop-hook and the daemon's busy local worker. Remote hosts
  advertise `remote_live_controls` separately. Only the current controller may
  enqueue them, and the accepting grant is checked again while applying settings;
  local takeover, release/regrant and host shutdown retire old queued changes.
  Rejections are diagnostic messages and cannot complete the active prompt.
  Authenticated Unix WebSocket tests exercise real generation polling, observer
  rejection, updated catalogs and matching main completion; daemon/worker tests
  cover current-generation grants and active-request preservation. Older engines
  retain ordinary dispatch. Polling blocked inside a tool may delay application;
  loaded-engine and physical UI interaction remain to verify.

- Graphical console hosts now feed remote buddy subscribers through the same
  original player and latest-only PNG slot as terminal hosts. A remote observer
  keeps playback active independently of the host window's focus lease. The
  console idle timer includes remote playback; leaving the final subscriber
  pauses it. Submitted/automatic main turns dismiss the remote image, and the
  idle poll republishes static frames afterward. Buddy command responses also
  mirror to remote viewers. Isolated fixtures cover focused/unfocused hosts,
  disconnect/reconnect, static restoration and dismissal without terminal escape
  output. Physical native/browser presentation remains to verify.

- Conversation replacement retires pending approvals, download progress,
  provider conversation bindings and the current suggestion while preserving
  the running engine's catalog. The reset record and durable control cleanup
  commit together. Sequenced resets apply once: reconnect replay cannot clear a
  newer question, suggestion or task view. Stale worker resets are ignored.
  Tests cover old approval rejection, catalog retention, duplicate reset replay
  and generation fencing.

- `/resume <session>` and `/switch <session>` now replay recent loaded history
  to remote viewers after the reset boundary. Console output is printed once
  locally, and the shared adoption path also serves terminal hosts. Reconnect
  scrollback retains the new history rather than the replaced conversation.
  History uses the existing six-turn/per-message display limits and bounded
  UTF-8-safe remote frames. Tests verify both commands, ordering, exact rendering
  without duplicates, replay replacement and history-monitor reset behavior.

- Local submissions use one JSON `prompt` envelope when `console_prompts` is
  advertised. Multiline prompts, command arguments, templates and shell scripts
  no longer depend on pipe-read batching. Protocol-shaped text stays opaque;
  pasted `dialog_reply`, `btw` or frame JSON cannot answer a question or execute
  another control. The daemon negotiates the capability again after restart
  and retains raw transport for older engines. Tests cover byte-fragmented
  Unicode frames, multiple submissions, literal control objects, multiline
  slash arguments, startup/restart transport and legacy fallback.

- Local `/quit` and `/exit` now acknowledge intentional console shutdown with
  `+PLANK_EXITING` after cleanup. The daemon drains final output, requires both
  that acknowledgment and a successful exit, then retires the worker and starts
  a fresh generation while retaining conversation history. The next prompt can
  start another engine directly; old catalogs, dialogs, telemetry and exports
  cannot carry across the boundary. Unexpected exits still require recovery.
  Pipe draining after process exit is bounded so inherited descriptors cannot
  hang the worker indefinitely. Process tests cover both aliases, immediate
  subsequent input, final output and failed/unacknowledged exits; a real console
  subprocess verifies marker emission and plain EOF behavior.

- `!command` and `!!command` now execute directly in graphical console
  sessions instead of reaching inference as prompts. Both stream shell output;
  only single bang adds Plank's existing bounded, escaped result to model
  context. Runtime catalogs expose both forms in the command picker under the
  `shell_escapes` capability. Remote output splits long lines at Unicode
  boundaries. Stop/identified cancellation terminates the shell process group,
  completes the matching request and permits later input. A late stop also
  prevents GPU unload/re-execution. Tests exercise real shell output, context
  isolation, cancellation, frame bounds and catalog validation; existing GPU
  handoff behavior remains shared with the terminal path.

- Goal iterations and closing notices now reach remote viewers, with task
  snapshots at iteration boundaries and after the goal settles. Remote Stop or
  identified cancellation during work or verdict generation ends the current
  iteration without accepting a late terminal verdict or starting another pass.
  Cancelled provider errors use the interrupted outcome; command completion
  retires cancellation so the next prompt works normally. The TUI verdict path
  also rejects late stops. Model-free tests cover work/verdict cancellation,
  provider failures, task snapshots, matching completion and subsequent input.

- Console compaction now forwards its start, success, interrupted and empty-
  summary notices, plus spill/micro-compaction diagnostics. Remote Stop and
  identified cancellation reach its generation stop hook; cancellation after
  the final callback discards the summary even when engine statistics say the
  pass completed. Cancelled provider errors use the interrupted outcome.
  Interrupted commands retain the original conversation, retire their matching
  completion and permit a clean retry. The TUI path applies the same late-stop
  rule. Model-free tests cover both paths, cancelled failures, retry and empty
  summaries; physical loaded-engine interaction remains to verify.

- Graphical console sessions can now start a loopback host with `/rc on` or
  `/rc ask`, inspect `/rc status`, grant requests with `/grant`, and stop with
  `/rc off`. Settings → Host gates local start actions on `console_host`; older
  engines remain disabled. Remote attachments cannot start another host, but
  can inspect or explicitly stop their current one. Actions still prepare a
  command for review and Send. An authenticated WebSocket over a Unix socket
  exercises request/grant, `/command` without inference, an actual scripted
  agent question and reply, matching completion, and local takeover rejecting
  later remote input. A separate graphical-protocol subprocess checks toggle
  dispatch against an existing listener fixture and detachment on stop. Fresh
  TCP binding, loaded-engine startup/trust flows, blocking-tool cancellation and
  physical native/browser operation remain runtime verification work.

- Console turns now mirror tool activity, typed edit cards, task completions,
  hook warnings and hook-stop notices to attached remote viewers. Child notices
  remain in their sub-agent output instead of appearing again in the parent log.
  A model-free scripted edit verifies that one typed diff reaches the viewer;
  a child-output test checks routing isolation. Console pressure status,
  context-limit and loop diagnostics, micro-compaction and job-wakeup notices
  also forward typed events. Tool status and Markdown sinks follow the current
  attachment, including after stop/restart, and are installed for graphical
  console sessions without a terminal footer. Tests verify main/child diagnostic
  separation and attachment replacement. Lifecycle-hook callbacks now use an
  owned output sink for startup/resume, prompt submission, compaction, reset,
  Stop and exit. Explicit Stop-hook reasons are surfaced rather than discarded.
  Shell-backed tests verify warning delivery, bounded Stop feedback continuation,
  explicit stops before inference and child-output isolation. Other direct
  console notices and broader real-controller interaction still need coverage.

- Remote terminal buddy playback now has an explicit `buddy_stream` auth opt-in.
  The host retains one latest PNG outside the transcript bus, replay sequence
  and export queue. Completed terminal buffers determine visibility: covered
  placeholders and Warp markers hide the mirrored image, and entering a busy
  turn publishes a clear. Each viewer tracks its own slot revision; a slow or
  reconnecting viewer receives the latest state rather than queued animation.
  Replacement players with the same clip/frame indices still publish fresh PNGs.
  The daemon's observer connection opts in without requesting control, and uses
  the same bounded decode and compact idle overlay as local sessions. Controls
  remain controller-only. Graphics-free terminals, including tmux, can now run a
  remote-only player when a graphical subscriber is attached. Native kitty,
  Ghostty and Warp presentation remains preferred where available. Remote-only
  playback writes no terminal graphics or placeholder cells, hides for modal
  overlays, and requests animation ticks only while a subscriber receives frames.
  Tests cover actual authenticated Unix socket delivery, legacy opt-out, rejected
  authentication, coalescing, replay exclusion, overlay/busy buffers, replacement
  players, a graphics-free subprocess and the real remote-worker WebSocket adapter. Actual terminal/native/
  browser presentation and live installation remain to verify.

- Local graphical `/buddy`, `/buddy on|off` and `/buddy install [name]` now
  reuse the original clip player, background installer, repository picker and
  transparent PNG cache. Installation choices use the existing explicit question
  bridge. The idle console publishes only changed frames at the original 12 Hz;
  inactive graphical views stop frame production through the expiring focus
  mailbox. PNGs are limited to 256 KiB and 1024 pixels per edge. They remain
  transient telemetry, never transcript entries, replay history or attention
  events, and unselected sessions omit images from client snapshots. The client
  keeps one decoded image, renders it in a 120×100 overlay immediately above the
  composer while idle, and hides it during a turn or after reset/disconnection.
  Settings → Session offers compact controls that prepare commands for Send.
  `buddy_frames` gates older engines; protocol 41 carries the latest PNG.
  Tests cover original frame bytes and timing, graphical commands without kitty
  escapes, transparency/color conversion, transport, generation isolation and
  cleanup. Actual repository installation, native/browser presentation and
  graphics-free remote host presentation remain to verify. Remote playback is
  described above.

- `/notify unfocused` now uses the selected local session's graphical focus.
  Native activation and browser document focus/visibility publish ephemeral
  per-window leases; another focused window can keep its own session observed.
  Switching sessions releases the previous view. Leases expire after three
  seconds, and the engine's bounded private mailbox expires independently after
  five, so a closed/crashed client cannot suppress notifications indefinitely.
  The mailbox is refreshed outside the prompt queue and read at notification
  time, including while inference or a tool blocks agent polling. Always and
  Never retain their original behavior; stale, malformed and unreadable focus
  state permits unfocused notifications. Focus does not enter transcripts, alter
  running requests, or acknowledge attention. `gui_focus` gates older engines;
  client/daemon protocol 40 adds ephemeral viewing updates. Tests cover the real
  busy worker, disconnect expiry, multiple windows, session/generation isolation,
  and notification policy in an isolated engine subprocess. Remote terminal
  hosts retain their terminal focus; remote clients evaluate their own focus as
  described above. Physical native/browser notification interaction remains
  unfinished.

- Stop follow now bypasses the agent's ordinary command queue. Both the compact
  Jobs button and exact `/jobs unfollow` submission use an observer-scoped
  control when `job_output_cancel` is available. Local workers receive a private,
  atomically replaced mailbox; authenticated remote controllers use a separate
  protocol message. The observer checks cancellation at its one-second sample
  interval and publishes a final stopped snapshot. Stale observer identities,
  replaced worker generations and remote observers cannot cancel a new view.
  Stopping observation neither interrupts the agent nor kills the job, completes
  a main request, consumes its notification, or acknowledges unrelated attention.
  Protocol 39 adds the typed daemon control and optional observer identity;
  older hosts retain queued `/jobs unfollow`. Starting follow still requires
  ordinary dispatch. Model-free tests cover the real busy local worker process,
  independent engine observers, mailbox bounds and replacement, remote controller
  fencing, retries and preserving the active command completion identity.
  Physical native/browser interaction remains to verify.

- Jobs → Output offers Follow / Stop follow through `/jobs follow <id>` and
  `/jobs unfollow`. One scoped observer per job table samples the last 16 KiB
  every second independently of generation and blocked agent polling. Only
  changed snapshots publish, and live views scroll to the newest output.
  Stream EOF and read failure publish stopped state. Selecting a page, replacing
  the observer, resetting context or dropping the table cancels and joins it;
  observation never kills the job or consumes its completion notification.
  Startup publishes before background updates to preserve ordering. Tests cover
  EOF without agent polling, cancellation, read failure, context reset, legacy
  snapshots and preserving the client's busy state. `job_output_follow` gates
  older hosts; protocol 38 adds optional tail/following state. Starting follows
  uses the ordinary command queue; independent stopping is described above.
  Physical native/browser interaction remains unfinished.

- Jobs → Output now pages tracked job files through
  `/jobs output <id> --page N`. The existing bare output command still shows its
  final 32 KiB. The compact view offers previous/next, refresh, last page and
  Copy page; actions prepare commands for Send. Pages read a bounded 16 KiB
  window plus UTF-8 boundary overlap, preserve all valid text across pages, and
  explicitly mark replacement decoding of invalid bytes. Only the current
  agent's tracked job IDs can resolve to files; no client path is accepted.
  Reads do not consume completion notifications or alter parent messages.
  Tests reconstruct long Unicode files, bound binary decoding, exercise owned
  job command dispatch, and reject forged remote ranges. `job_output_pages`
  gates older hosts; protocol 37 carries the output snapshot. Continuous output
  control while blocked and actual native/browser interaction remain to verify.

- Completed child archives now belong to `Session` and persist in length-framed
  `sidechain` records inside the existing session file. Completion and outcome
  correction save through the store's fsync-and-rename path, using the parent's
  pre-fork transcript so child turns never become parent turns. Resume restores
  stable IDs and full text; clear/new starts with no archives, and session
  copies/deletion carry their archives together. Shared copy-on-write storage
  avoids copying full archives into ordinary generation snapshots. Tests cover
  cold adoption, durable retirement, Unicode/timestamps, session copying,
  deletion and a failed write preserving the last committed file. Existing
  archive-free files remain byte-identical on roundtrip. The existing inability
  to save parent sessions containing image embeddings still applies; failed
  archive saves retain memory and emit a diagnostic. A subprocess test now
  forcibly terminates the real model-free agent fixture during a later child
  and restores only the committed parent and completed history. Loaded-engine/
  native/browser recovery interaction remains to verify.

- Agents → History exposes the engine's last eight completed sidechains through
  `/subagent-history [run-id [--page N]]`. Stable IDs distinguish repeated names
  and survive older-run retirement. The read-only view stays in the 160-pixel
  panel, folds the run chooser after selection, and provides paging and Copy
  page. Each page carries at most sixteen 4 KiB Unicode-safe text pieces; full
  message text remains available across pages, including tool-result envelopes
  and empty messages. Image embeddings remain omitted by the underlying repro
  archive. Invalid/retired IDs and malformed pages are refused without changing
  the parent conversation. `subagent_history` gates older hosts; protocol 36
  carries the requested metric. Tests reconstruct long Unicode conversations,
  exercise actual archive retirement and console commands, and verify remote
  ingestion rejects forged selection. Actual host-restart/native/browser history
  interaction remains to verify.

- Frames now offer a collapsible Text input row using the toolkit's editable
  Input, with explicit Insert, Enter and Backspace actions. This provides a
  commitment path for Unicode/IME and software keyboards without interpreting
  text as conversation prompts. One frame-scoped event contains at most 128
  printable Unicode characters and 512 bytes; all text is validated before any
  guest call. Hosts deliver the original characters in order and stop on close
  or trap. Console frames, TUI mirrors and arcade consumers handle the event.
  Controller, grant, frame identity and generation checks use the existing
  bounded input channel. Drafts retire with the frame; observers cannot edit.
  `frame_text` gates the row for older hosts, and client/daemon protocol 35
  supports the new event. Model-free tests cover Unicode, bounds, redaction,
  close/trap boundaries, console/TUI dispatch and real worker transport. Physical
  IME/mobile keyboard interaction and guest execution remain to verify.

- Native/browser frame scenes now retain bold cells through a half-unit second
  glyph impression. This synthetic weight compensates for Scene2D's missing
  per-node font weight; it does not change Unicode content, cell advances,
  colors or pointer mapping. All backgrounds still paint before glyphs, so
  adjacent cells cannot erase wide characters. Regression tests check mixed
  bold/regular Unicode cells, scene validity and unchanged hit coordinates.
  Physical rendering at different display scales remains to verify, and a true
  bold font face remains preferable when the toolkit exposes per-node weight.

- Settings now groups Options, Memory, Profile, Session and Diagnostics inside
  the existing 160-pixel surface. Memory exposes editing, its change log,
  project/user note preparation, forget and calibration; Profile exposes the
  shared running-profile editor and repository setup. Session exposes hints
  and explicit notification modes. Diagnostics exposes help, version, prompt
  history, reproducer generation and debug toggles. All actions prepare the
  composer and require explicit Send; reported command availability and remote
  observer state disable unsupported actions. Submitted slash commands select
  the corresponding section. `/notify always|unfocused|never` now uses the
  existing mode policy while preserving legacy on/off and bare toggling.
  `notify_modes` gates those buttons on older hosts. Notification mode command
  tests verify policy changes without generation. Delivery remains the engine's
  notification implementation; local graphical focus now reaches its unfocused
  policy as described above. Native/browser interaction remains to verify.

- Settings and Extensions → Plugins now expose a compact component selector.
  Loaded components with declared options receive separate 32-field pages;
  similar IDs such as `dev` and `dev.demo` never share a page. Paging, refresh
  and setters retain the selected component. `/config --component <id> --page N`
  requests a page, and `/config --component <id> set <option> <value>` edits an
  option through the existing declaration validator. Global settings remain
  accessible. Controls are capability-gated, and edits require explicit Send.
  Component options now persist in `pluginConfig`, preserving unknown options;
  they apply at the next component initialization. Tests exercise actual saves,
  reloads, invalid-value rejection, scope retention and legacy snapshots.
  Protocol 34 carries scope and bounded component suggestions. Actual guest
  initialization and native/browser selector interaction remain to verify.

- Saved conversations and branches now have inline forms for current-conversation
  rename/tag, rating notes, checkpoint names and saved-search scope. They stay
  inside the existing 160-pixel panels, focus their input on opening, and prepare
  commands for explicit Send. Rename follows the engine's 64-character ASCII
  name rules and reserved cache prefix. Empty tags do not prepare an accidental
  clear; the separate Clear tag action remains. Known checkpoint collisions say
  Prepare replacement, and page-option syntax cannot masquerade as creation.
  Saved search now passes complete multi-word queries to the real index instead
  of silently dropping later words. This project / All projects controls the
  trailing `--all` option; older hosts without phrase support accept one term.
  Command tests cover names, scope, reserved options and control-character
  rejection. Model-free engine tests verify phrase parsing and real scoped-index
  results. Native/browser forms and loaded-engine rename confirmation, ratings
  and checkpoint lifecycle still require interactive verification.

- TUI-host arcade games now publish to attached graphical clients from both
  idle and busy render loops. The controller uses the same keyboard/mouse/
  resize/close channel; physical keys and clicks take local control and invalidate
  queued remote keys. Physical hover cannot steer a remotely controlled game.
  The publisher keeps one latest grid at most 20 Hz, outside transcript replay.
  Editor frames have publication priority checked under the frame-slot lock;
  arcade consumers cannot drain another frame's input FIFO. Questions and tool
  modals suspend the arcade identity, and resumed/replaced games receive fresh
  identities. Source TUI sound/veiling remains unchanged; graphical views stay
  silent and opaque. Automatic idle screensavers are not mirrored as games.
  Model-free tests cover local takeover, resize, editor priority, preserved editor
  input, stale identities, game switching and screensaver exclusion. Actual TUI/
  TCP/native/browser gameplay and loaded-engine modal handoff remain to verify.
  Each socket also tracks its last visible frame identity. If coalescing skips
  an intervening open/close, the close notification retires that viewer's previous
  surface rather than leaving it stale. A two-viewer regression covers this
  without weakening client identity fencing or changing the wire protocol.

- Idle graphical console sessions can play built-in Pelota, Breakout and Matrix in
  Activity → Frames. Capability-gated buttons prepare the hidden game commands;
  New game selects the existing `new` argument, while ordinary reopening resumes
  parked matches. The host reuses the original simulation, glyphs, banner and
  keyboard/mouse controls, publishing at most 20 frames per second through the
  same bounded frame channel as WASM components. Resize and closure are scoped
  to the current frame identity. Queued prompts stay outside the game, and EOF,
  interruption and output failure retire its surface. Games are explicitly
  silent and opaque: terminal BEL, sound and transcript-veiling toggles are
  suppressed in graphical mode and omitted from its footer.
  Model-free tests run all three games and cover resize, input, stale IDs,
  parked state, prompt preservation and failure cleanup. The original terminal
  behavior and hidden command catalog remain intact. Starting graphical-console
  games during an active agent turn, graphical audio/transcript veiling and
  real native/browser gameplay remain unfinished. TUI-host idle/busy mirroring
  uses the shared channel described above.

- Tasks & goal now provides inline Add, Subject, Active label and Goal forms
  inside the existing 160-pixel activity surface. Fields receive focus when
  opened. Prepare copies a validated command into the composer for review and
  explicit Send; status/report/clear actions retain their existing controls.
  Goal creation exposes the positive iteration cap (default 20) and always
  supplies `--max`, so an objective named `clear` cannot clear the current goal.
  Task forms reject multi-line/control-character input and enforce the engine's
  1024-byte argument limit. Unchanged labels do not prepare a replacement;
  changed/missing task rows disable an already open edit form. Cancel closes the
  form and restores composer focus. Unit tests cover command grammar, Unicode
  bounds, reserved goal objectives, task changes and unchanged-label protection.
  Actual loaded-engine and native/browser form interaction remain to verify.

- Activity → Files provides a newest-first, eight-row paged inventory of verified
  conversation exports, Insights reports and HTML previews. Every retained file
  keeps its own authenticated download and sandbox-preview actions, even when a
  newer artifact has the same category or filename. `/export` and `/open` select
  this compact 160-pixel panel. The daemon retains up to 128 completed files and
  approximately 512 MiB per session generation; it keeps the newest complete file
  even when that file exceeds the byte budget. Oldest files retire first, with
  a visible retirement count. Existing readers can finish; new requests for
  retired IDs fail. Reset, generation replacement and daemon exit clear history.
  Snapshots contain descriptors only, and latest-category fields are rebuilt
  from the retained inventory. Older latest-only snapshots remain readable.
  Model-free tests cover ordering, limits, reader isolation and generation
  fencing; a real worker/HTTP test verifies repeated exports, older-file
  retrieval and reset revocation. Client/daemon protocol 33 adds this inventory.
  Native/browser file-picker and preview interaction remain to verify.

- Compact document editing supports document-wide selection with Shift-arrows
  and Shift-click, including across twelve-line pages. Up/down retains the
  desired column through shorter lines. Cmd-A (Ctrl-A outside macOS), copy,
  cut, paste, deletion, Enter and Tab operate on the draft's selected range.
  Selected segments remain highlighted in unfocused rows. Replacements form one
  undoable splice and retain unaffected line identities, Unicode and final
  newlines. Multi-line Find selects its entire match. Native text/IME replacement
  uses the same draft splice; the originating input cannot overwrite its relocated
  cursor. Selection is transient and excluded from saved draft preferences.
  Model-free tests cover forward/reverse selection, paging, paste, pointer
  extension, Unicode, undo/redo and recovery. Unmodified row dragging and page
  hover are now implemented as described above. Larger files and physical
  native/browser/IME interaction remain to verify.

- Remote-requested WASM frames use the same compact Frames panel. Clients
  negotiate `frame_stream` during authentication; older clients receive no new
  frame messages. The host keeps one latest grid and a bounded input FIFO outside
  transcript/reconnect replay. Snapshots coalesce; a connecting viewer receives
  the current grid. Only the controller can enqueue input, and consuming it
  rechecks the accepting grant generation, so release/reclaim or local takeover
  retires queued keys. Native/browser clients preserve main command completion
  while sending frame input; rejection does not complete a turn. A controller
  can release/reclaim while a frame is open, and observers render without input.
  Console hosts drive frames without terminal input. Remote TUI requests use a
  modal host rendering path with local input takeover and remote keyboard/mouse/
  resize/close handling. Remote-owned tool editors choose that inline remote
  driver while local TUI tool editors retain their original bridge. The file
  grant, sidechain, containment, stale-target and atomic-write checks remain.
  Controller/grant/FIFO and opt-in wire tests plus duplex WebSocket tests cover
  handoff, observer input suppression, stale IDs, parent completion and closure.
  Client/daemon protocol 32 supports these controls. Locally opened TUI frames,
  including tool-lent editors, now share the same snapshot/input channel. A
  lifetime guard retires their wire identity on closure or TUI exit. Local input
  takes control and discards the previous controller's queued keys; pointer hover
  alone does not seize control. Publications are capped at 20 Hz. Large terminals
  expose a bounded top-left viewport; controller resize overrides the frame size
  until local input restores terminal sizing. Model-free tests cover snapshots,
  clipping/style preservation, keys, mouse, resize, close, cancellation and local
  takeover. Real guest/TCP/native/browser/mobile
  interaction and tool-editor writeback still require verification.

- Local graphical `/frame <id> [face]` runs the existing WASM session without a
  terminal. Activity → Frames keeps its surface at 160 pixels by default, with
  Focus, Compact, Expand, scroll and Close controls. Bare `/frame` lists components
  and loads the compact inventory. Toolkit `GameSurface` retains scene objects,
  maps pointer/touch hit testing to character cells, and shares native/browser
  rendering. Scoped raw keyboard handling retains typed characters, navigation,
  Tab and Ctrl/Alt editor chords; platform menu shortcuts remain available.
  Snapshots include foreground/background colors and bounded Unicode glyphs.
  Painting backgrounds before text preserves wide glyphs. Source input uses a
  live read instead of paste joining, preserves queued prompts and bounds batches.
  Frame ticks and client polling run at most 20 Hz while active; client input
  feedback is batched. Generations and unique frame IDs fence input and close
  events. New frames request session attention; accepted input acknowledges it.
  Retirement, traps and EOF clear the surface; disconnected editors fail before
  their staged changes can be written. Tool-opened frame editors reuse this
  driver and the existing grant, containment and atomic-write checks. Loaded
  command aliases can request a frame at their completion boundary.
  Model-free host tests cover input, resize, cleanup and prompt preservation;
  a real worker fixture verifies delivery, stale ownership, close/reopen, crash
  retirement and absence of frame input from the transcript. Client/daemon
  protocol 31 adds this surface. `just build-plank-gui` enables WASM, built-in
  editing and Gemma without optional document/browser integrations. True bold font-face fidelity, mobile text/IME input
  and physical interaction remain unfinished. Bundled guest execution is unverified: its pinned
  `plank-guest-support` Git dependency was unavailable in the offline cache.

- HTML `/open` transfers a separate verified Preview artifact to graphical clients,
  including remote console and TUI hosts. It never opens a host browser for a
  graphical/remote request. UTF-8 HTML files are bounded to 32 MiB; invalid,
  oversized, disconnected or unsupported transfers report failure. Preview does
  not overwrite conversation exports or Insights artifacts. Artifact rows place
  Preview beside Download; preparation changes it to Open preview for an explicit
  click. Native clients use owned temporary files, browser clients use Blob URLs.
  Both open a sandboxed browser page with scripts, forms and external resources
  disabled; standalone styles and data images work. File length and SHA-256 are
  rechecked before wrapping HTML. Prepared pages are bounded to one and retired
  when the artifact/session generation changes. Model-free tests cover remote
  command transfer, unchanged files/context, corruption, artifact separation,
  reattachment and reset fencing. A real console worker fixture verifies authenticated
  retrieval and reset revocation for independent preview and export files. Native/browser rendering and transport still
  need interactive verification. Embedded GPUI previews and script-driven chart
  parity remain outstanding. Client/daemon protocol 30 adds Preview ownership.

- Prompt suggestions use the existing quiet, tool-free sidechain pass and appear
  inside an empty composer as a single-row hint. Tab or click copies the offer
  into an editable draft; it never sends by itself. Typing dismisses that offer
  revision, so repeated snapshots cannot revive it in the same composer state.
  Suggestions hide while busy, disconnected, observing, editing a document,
  answering a question or using the command picker. Console clients generate
  once after a completed turn at an idle slot; existing TUI remote hosts publish
  their offers too. Runtime controls carry optional, bounded suggestion metadata,
  preserving older-engine compatibility. Prefix/length validation, generation
  fencing and context-depth checks reject executable or stale hints. Model-free
  tests cover quiet generation, parent preservation, console scheduling and
  validation; loaded-engine and native/browser acceptance remain to verify.

- Identified input completion retires command/prompt metadata atomically, so
  cancellation and exit cannot acknowledge an input twice or change its kind.
  Console and TUI exit paths finish the current command and reject undispatched
  batch inputs explicitly. Shutdown drains queued completion frames before
  `bye` and joins connection threads before process replacement. Header/auth
  waits observe shutdown instead of using their full setup deadline. Model-free
  regressions cover completion races, exit batches, wire ordering and connection
  retirement. Actual host/client restart verification remains outstanding.

- Remote engine switching is available through the existing Choose action when
  a Unix host advertises `remote_restart`. The engine saves its conversation,
  preserves listener settings through private IPC and sends an instance-bound
  `restarting` frame after completions and before `bye`. The daemon retires live
  controls/dialogs/telemetry/files, retains transcript and request receipts, and
  reconnects as an observer without replaying input. It waits for a different
  instance rather than consuming old replay; a fresh grant is required to answer
  startup choices. Retry is bounded to 90 seconds and accepts Stop; unsent input
  is refused rather than carried into the replacement. Missing completion or a
  mismatched restart identity retains recovery semantics. Missing `bye` has a
  bounded close wait. Source wire/recipe tests and duplex WebSocket regressions
  cover restart, choice answers, fresh input, namespace changes and refusal paths.
  Windows host handover and actual engine/TCP/native/browser verification remain.

- The controller can explicitly stop a remote host with `/rc off` or
  `/remote-control off`. Console and TUI dispatch emit the result and completion
  before closing the listener. A policy-locked stop fence refuses new inputs;
  accepted later batch inputs receive explicit rejection and completion.
  The daemon handles `bye` as a clean detach only after every submitted input
  completes, clearing stale attachment telemetry. Missing completion or queued
  input retains the uncertain-input failure path. Model-free console/policy and
  duplex WebSocket regressions cover these behaviors; actual TCP/native/browser
  stop interaction remains to verify.

- Saving a changed remote profile offers an explicit restart on Unix hosts with
  an owned listener. Later retains saved files and keeps the current engine
  running. Restart now saves the conversation and uses the existing listener
  handover/reconnect path without opening an engine chooser. Console hosts can
  edit profiles through the same remote editor bridge without local GUI mode.
  Unchanged edits do not prompt. Both GUI and remote edits reject stale source
  files before validation/writing; invalid profiles leave files unchanged.
  Cancellation before/during confirmation prevents restarting, including after
  transient interrupt reset. Conversation-save failure retains the already saved
  profile and reports the failure without restarting. Model-free filesystem and
  restart-plan tests cover these paths; actual engine/TCP/UI verification remains.
- `/command` and `/commands` are recognized consistently by the shared CLI
  command classifier as well as the console/TUI handlers and runtime catalog.
  Built-in recognition now uses the advertised catalog, including argument
  forms such as `/search query` and `/list 2`, with separate handling for hidden
  games and named subagents.

- `/btw` output now has typed side-answer frames instead of being rendered as a
  main answer. Plain-console streams keep ordinary CLI bytes, suppress duplicate
  GUI stdout, restore their aside scope on return and retain the parent transcript
  and context counter. TUI remote mirroring tags side-pane output, including
  explicitly addressed forked-aside events. Native/browser transcripts label side
  answers separately and fold their reasoning, tool banners and notices. UTF-8
  console frames are bounded; replay preserves their side roles. Activity → Asides
  displays side answers in the existing 160-pixel panel, with Ask, Older and Live
  controls. Side history pages independently of the main transcript; side chunks
  cannot displace main turns from its page. Reasoning, tool banners and notices
  remain folded. Model-free tests
  cover parent preservation, subsequent main output, bounded Unicode frames,
  deduplication, independent pagination, session isolation and worker-generation
  fencing. Remote `/btw` submissions use Plank's dedicated ephemeral frame while
  idle or busy, without changing the main turn's completion identity, state or
  transcript. Observers can ask side questions without requesting control, matching
  Plank's policy. Ask and Send remain available for remote side questions during a
  main turn; other busy-session commands remain disabled. Accepted request IDs are
  deduplicated, including when the queue is full; queue refusals remain retryable.
  Replaced workers cannot forward a queued side question. Model-free WebSocket
  regressions cover owned active turns, observer access, typed answers and main
  completion isolation; daemon regressions cover receipt/queue behavior. Actual
  engine/native/browser interaction remains to verify.
  The owned local console inbox now recognizes typed `btw` frames independently
  of main prompts and dialog replies. It retains fragmented UTF-8, bounds each
  question to 64 KiB and the FIFO to 20 questions, and shares the existing 4 MiB
  pending-input budget. Overflow refuses the new question without evicting accepted
  ones. Idle dispatch consumes buffered questions as `/btw` before ordinary input;
  editor/approval replies cannot accidentally consume them. Malformed typed frames
  are refused instead of becoming model prompts, and Debug omits question contents.
  Input regressions cover fragmentation, editor isolation, bounds and retry after
  draining. Rebuilt GUI consoles advertise `console_asides`; only those local
  engines accept independent side input from the daemon. Ask and Send stay enabled
  for `/btw` while the main turn runs. The worker writes a typed frame without
  replacing the main input or changing its state. Generation probes stdin at most
  20 times per second, without the ordinary paste-joining wait. With suspend enabled
  and a flat-prompt backend implementing asides, it freezes the main pass, answers
  through the backend's fork/restore path, and resumes the same renderer, partial
  reply and repeat guard. Other backends answer at a generation boundary. Side
  output does not replace the main live counters. A stop during the aside prevents
  another main generation and clears queued questions; side failures are reported
  separately. Model-free regressions cover suspension, boundary fallback, preserved
  main/side streams and cancellation. A real Rust process fixture verifies busy
  delivery, capability negotiation, deduplication and unchanged main completion.
  Interleaved multiplexing parity, long blocking-tool input latency and actual
  loaded-backend/native/browser interaction remain to verify or implement.
  Repeated suspensions now combine only the main segments' token, usage and
  speculative-decoding figures for the main pass report. Throughput uses their
  combined measured durations; missing timing reports zero as an unmeasured rate
  instead of reusing the last segment's speed. Whole-session accounting still charges
  each main and successful side pass once. An open reply remains a continuation
  even when a side question arrives before any new main text. Regressions cover
  repeated questions, separated spend, resumed prefixes and a failed side answer
  that does not abort or pollute the main reply.

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
  Actual child-history host-restart recovery, elapsed ticks while blocked in tools and actual engine/native/browser
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
  and clean retry. Blocking-tool and loaded-engine host/controller verification
  remain outstanding.
- Model → Engines shows the host's published and local catalog in 32-row pages,
  including versions, notes, installed/download/downloading/staged states and
  unavailable reasons. Build support, RAM, free download space and the current
  named engine accompany the report. `/engines --page N` uses the startup
  chooser's fit rules without loading a model or restarting. Unresolvable
  entries remain visible as disabled. Labels are bounded; oversized display
  names are abbreviated with row identifiers, while the chooser keeps complete
  names. Load and paging prepare commands for explicit Send. Choose opens the
  host chooser when its capabilities permit switching. Older remote hosts retain
  inspection only. Catalogs use the generation-fenced report
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
  still need loaded-engine end-to-end verification.
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
  daemon protocol introduced this in version 33. Local graphical hosting is now
  enabled with authenticated ownership/handoff coverage; remaining startup/trust/
  modal runtime checks, blocking interruption and notice parity remain open.
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
  editor before the engine becomes ready. Loaded-engine startup/trust/modal
  runtime verification remains outstanding.
- Remote ingress permits the exact read-only `/rc status` and
  `/remote-control status` reports, plus explicit `off` after controller
  authentication. Starting/toggling a host and self-grant commands remain
  local-only; extra arguments cannot bypass that guard.
- The console-host bridge's output layer mirrors parsed answer, reasoning,
  tool, and error events to an attached remote bus while retaining the primary
  terminal renderer. Transient preview rewriting stays local. Console slash
  reports also mirror their printed bytes without process-wide stdout
  redirection. Graphical console mode now supports `/rc`; unattended plain
  consoles retain the terminal/graphical-host requirement. The idle loop drains remote input snapshots
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
  retain text reports. Remote switching requires the host's restart capability.
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
  reports 18 capabilities, including independent console aside input and attachment support separately from model
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
- Rebuilt remote servers advertise an optional, noncredential replay-instance ID
  in their authenticated hello. The daemon persists the binding independently of
  connection IDs. Reattaching to the same instance keeps deduplication; a changed
  instance clears seen IDs before its snapshot arrives, so restarted sequence
  numbers cannot silently hide new output. Old transcript entries and request
  receipts remain, with one compact context-boundary note. Host controls, dialogs,
  live metadata and downloads are retired on the change. Malformed identities and
  replaced workers cannot alter a newer binding. Legacy hosts retain unscoped replay
  behavior and cannot identify a restart. Model-free socket/store regressions cover
  same-instance replay, restarted IDs, persistence, legacy transitions, session
  isolation and stale-data cleanup without resubmitting inputs. Real host/client
  restart and reconnect verification remains outstanding. Remote state Debug
  omits credentials and retained input contents.
  Unix listener handover now has an owned, bounded recipe and anonymous socket
  transport. It carries the address, bearer token, base access policy, origins and
  queue limit without copying live grants, queued inputs or replay identity.
  Commands own their inherited descriptor; dropping the prepared packet cannot
  invalidate it. Direct exec failure releases that private copy. Only a descriptor
  number belongs in the internal startup argument, which is consumed before launch
  arguments are recorded. Packet reads have a deadline and bound, reject invalid
  descriptors, and avoid Darwin's unsupported receive-timeout case. Debug and
  decoding diagnostics omit credentials. Model-free tests include a real child
  process, failed launches, bounds, stalled packets and argument consumption.
  Interactive and console exits capture the listener recipe before dropping the
  old agent. Restart execution uses the private packet; replacement startup
  consumes it before recording launch arguments and restores the same endpoint
  before loading the model. Failure aborts rather than silently changing the port
  or credentials. The owned listener moves into the replacement agent before
  session restoration, with fresh replay, input and controller state. Existing
  public runners retain their no-listener behavior. Startup argument regressions
  pass; the real listener restoration regression explicitly skips when sandbox
  policy forbids TCP binding. Pre-engine selection now uses paged structured
  questions on the restored listener. Terminal users can answer the same question
  locally; without a terminal the matching controller reply is polled without
  reading prompt bytes from stdin. Explicit decline, interruption and every
  accepted answer close the question identity. Tests cover page navigation,
  invalid/stale replies, preserved queued prompts and diagnostic/one-shot exclusions.
  Managed-engine startup downloads now mirror Leave downloading and Cancel download
  through the same question guard, while preserving the local terminal wait screen.
  Decline, interruption and leaving keep the helper running; explicit cancellation
  writes the existing keep-partials flag. Completion/failure closes the question,
  and completed helper state wins over a late cancel answer. Existing bounded
  download snapshots publish changed progress at most once per second and terminal
  results immediately. Tests cover these choices, progress coalescing, stale replies,
  queued prompt preservation and cancellation-write failure. Startup profile
  install/update confirmations now use structured consent on the restored listener
  or the local GUI's pre-agent question protocol.
  No is the initial choice; only a matching explicit Yes permits the change.
  Component trust offers also reach restored console hosts without requiring the
  GUI protocol flag, and their outcomes are mirrored to remote clients. Diagnostics,
  one-shots and chart/quiet modes do not open remote startup confirmations. Tests
  cover safe defaults, rejected stale/malformed replies, explicit decline,
  interruption and preservation of queued consent-looking prompts. Filesystem
  regressions verify that declined installs write nothing, declined updates preserve
  local edits, approved updates install the requested version, and current/unattended
  profiles do not ask again. The GUI reply reader preserves later prompt bytes.
  Loaded-component trust and actual GUI/remote consent flows still need runtime verification.
  Foreground local-catalog URL downloads now use an owned fetch process and the
  compact cancel dialog on GUI and remote hosts. Its bounded stdin recipe carries
  the URL privately; metadata carries only file size. Cancellation kills and reaps
  the worker, retains partial files, and closes the question. A stdin lifeline
  terminates the worker when its parent exits, including blocked network calls.
  GUI reply polling preserves fragmented frames and later prompt bytes. Completed
  downloads require a matching positive content length before finalization;
  incomplete/unknown-length files stay partial, and an existing destination is
  preserved. Foreground ownership overrides detached-helper polling during the
  transfer and retires on exit. Protocol 29 carries that ownership; native/browser
  monitors hide helper commands and point to the current cancel dialog. Model-free
  tests cover real subprocess completion/cancellation/parent loss, packet bounds,
  finalization, remote dialog closure and generation-fenced ownership ingestion.
  Real HTTP transfers and native/browser interaction remain to verify. Remote
  restart completion and observer reconnection now have model-free coverage.
  Base policies that
  require a local grant still require local authorization after restart.
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
  changed during editing. Streamed original opens accept text up to 32 MiB
  (legacy inline opens remain limited to 256 KiB); negotiated chunked Saves
  accept edited drafts up to 32 MiB.
- Editors have Undo/Redo buttons, Cmd/Ctrl-Z and Shift-Z shortcuts, and direct
  line-number navigation. Consecutive typing is grouped; pasted and removed
  lines retain their identities through undo. Draft history survives recovery
  and uses a combined undo/redo budget of 128 changes and 4 MiB of retained
  change data, including shared text chunks and piece/vector capacity. Recovery
  trims older oversized journals without changing draft text. A single edit
  exceeding the budget remains in the draft but cannot be undone. Reconstructing
  text avoids a temporary per-line reference array. Streamed larger-file opening
  and shared remote original bodies are implemented; legacy inline opens retain
  their 256 KiB limit.
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
  selection now uses shared gestures; streamed larger-file opening is implemented,
  while real native/browser focus interaction remains unverified.
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
  checked before saving. Changed remote profiles offer Later / Restart now when
  listener handover is supported, with a fresh control grant after reconnecting.
  Remote HTML uses the verified Preview artifact and client-side sandboxed browser page;
  embedded rendering remains incomplete.


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
The client/daemon protocol is now version 44, including separate revision-scoped original-document reads, checksummed UTF-8 editor uploads, remote completion notifications, graphical focus leases, latest buddy PNGs, foreground-download ownership, delegated-run snapshots, live progress, rendered tails and reports, paged engine inventory, saved-session activity heatmaps, required remote ownership fencing,
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
  lengths and checksums before publishing a verified export descriptor. Reset,
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
- Loaded-engine remote-turn and grant-handoff runtime verification, remote WASM
  frame rendering/input, and native/browser/mobile frame interaction.
- Console hosting must verify local/remote arbitration, complete startup/trust
  and remaining modal paths, and verify shared questions/editors with real
  controllers beyond model-free fixtures.
- Larger-file editor parity and real native/browser/IME focus, pointer dragging,
  scrolling, selection and navigation verification.
- Verify runtime capability reporting with actual loaded engines, including
  vision, GPU reopen recipes and backend concurrency.
- Live metrics and Insights chart runtime verification, full Insights HTML layout parity, loaded-engine task/goal form verification and richer job interaction,
  richer branch/checkpoint and saved-session management, component initialization
  verification and restart management,
  broader model/feature availability, and native/browser artifact inventory interaction.
- Windows host restart handover, embedded HTML previews, and remaining modal question paths
  must match local behavior.
- Verify command results with the real engine, interactive response lifecycle,
  crash/reconnect behavior, native UI flows, and browser UI flows.

## Verification

`just qa-ui` launches an isolated real client/daemon pair with bundled Plank's
model-free engine, disposable home/workspace/state and a transcript sample.
`nplank-ui-qa --prepare-only` prepares and checks its `/command` dispatcher
without a listener or window. This provides native interaction coverage without
using normal saved workspaces or agents; it does not verify loaded-model behavior.
The fixture includes `large-editor.rs`: `/open large-editor.rs` exercises chunked
opening, paged editing and chunked Save without requiring model inference.
The execution sandbox rejects both TCP and Unix listeners, so the full launcher
must run from a normal desktop terminal before physical UI checks can proceed.

`just check-plank-controls` runs focused model-free console, catalog, editor,
question, and remote-control regressions. Client tests check catalog validation,
generation fencing, explicit edits and answers, and remote completion matching.
Full control coverage remains incomplete until the requirements above are
implemented and verified.
