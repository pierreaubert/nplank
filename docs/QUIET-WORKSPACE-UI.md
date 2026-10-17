# Quiet workspace UI implementation

The requested end state is a keyboard-first native workspace with project/worktree
grouping, readable Markdown conversations, structured inline questions, and minimal
chrome. Compact screens retain a status rail when navigation hides.

## Acceptance checklist

- [x] Responsive navigation policy reserves room for conversation content.
- [x] Persist Auto / Pinned / Hidden navigation preferences and merge concurrent saves.
- [x] Collapsed navigation retains a status rail and opens as an overlay.
- [x] Green means no user action; yellow means pending choice; red means intervention.
- [x] Disconnection has an explicit unknown state; hidden nonarchived sessions count.
- [x] Selecting a session dismisses the overlay; Escape dismisses it without losing drafts.
- [ ] Verify the native layout and touch/keyboard behavior in a running app.
- [x] Workspace → project → worktree → agent session hierarchy.
- [x] Durable checkout association; agent execution, metadata, terminals, tools use it.
- [x] Discover/attach existing worktrees, create from branches, inspect changes, remove safely.
- [x] Distinguish closing tabs, archiving sessions, and removing checkouts.
- [x] Session tabs preserve draft, scroll position, and agent context.
- [x] Register application actions with gpui-toolkit keybindings, presets, overrides,
      conflict detection, palette discovery, and contextual focus behavior.
- [x] Keyboard navigation covers projects, worktrees, sessions, transcript, and questions.
- [x] Refine Markdown conversation layout and folded tool activity; keep failures visible.
- [x] Inline structured questions offer aligned options, explicit confirmation, and free text
      where supported by the provider; asynchronous questions never steal focus.
- [x] Move optional diagnostics/meters out of the default conversation chrome.
- [x] Update user documentation and run native build, tests, formatting, and Clippy.

## Current evidence

Navigation implementation: `src/navigation.rs`, `src/ui.rs`, with status
regressions in `src/fleet.rs`. Worktree operations live in `src/worktrees.rs`, with
durable records in `Store` and protocol commands in `Daemon`. Protocol version is 45;
database schema is 5. `cargo clippy --all-targets -- -D warnings` passes.
`cargo test --lib --test worktrees --test documents` passes 243 tests across three suites.
Native visual validation is partial; see the limitations below.

Implemented worktree UI: each workspace offers a Projects & worktrees manager,
discovery/attachment, new branch + checkout creation, new agent in checkout, and
confirmed removal. Sidebar session rows group by project common directory and checkout;
conversation tabs show peer sessions in the same checkout. Scroll handles are retained
per session and are not attached to transient empty transcripts during switching.
The checklist records implemented behavior and automated evidence; full native interaction
validation remains separate. The manager now includes an authenticated daemon-side changes inspector (file status,
staged diff, unstaged diff) and an explicit local-terminal action even for empty
checkouts. Git reads are bounded, external diff/textconv drivers are disabled, and
large inline previews are clearly limited. Local checkout identity verification runs
off the UI thread before spawning a terminal.

Regression evidence covers Git porcelain paths (including spaces/newlines), detached and
locked records, linked project identity, refusal to delete untracked files, branch
retention on removal, legacy-session attachment, persistence across Store reopen,
missing/removed checkout refusal, and removal guards across multiple workspaces.

The `--no-default-features` configuration now passes 142 tests across the same
three suites. The macOS shim needed the explicit `NSView` dependency feature to
expose `NSStatusItem::button` without GUI feature unification.

The application now uses `src/shortcuts.rs` and gpui-toolkit's keybinding registry
for palette discovery, presets, overrides, conflict checking, and per-window dispatch.
Unbound commands remain available in the palette. Navigator-only shortcuts use a
dedicated focus target so they do not override Enter on sidebar buttons. The rail
aggregates nonarchived sessions, including hidden workspaces and closed tabs.

Native question forms now use radio/checkbox controls with explicit confirmation,
exclusive decline for multi-select, and draft keys hashed from the full requirement.
Confirm revalidates the current generation and requirement. Regression tests cover
revised questions and exclusive selection. Questions are above the composer in a
bounded scroll area and do not request focus. Free text is not shown for providers
whose protocol only accepts listed choices. `RadioGroup::wrap_labels` was added to
the sibling gpui-toolkit with its previous truncation behavior retained by default.
Diagnostics counters and titlebar meters are opt-in.

Native QA launcher builds and verifies 76 Plank controls without loading model weights.
It now packages a distinct disposable `NPlank QA.app` on macOS, avoiding accidental
selection of the installed app. The application palette shortcut opened its accessible
input, filtering accepted text, and executing Toggle navigator persisted `navigation:
hidden` in the isolated preferences. Screenshots did not reflect subsequent state
changes, and resize automation failed with `noWindowsAvailable`; do not count this as
successful visual validation. Investigate native rendering/capture and repeat compact
layout, focus, questions, and worktree interaction checks. Both temporary QA launchers
were stopped through their owned cleanup path. The installed NPlank app remains open:
automatic approval review rejected quitting it because of possible user state.

## Worktree integration findings

`Session.worktree_id` now resolves to a durable `Worktree` record. Legacy sessions keep
their original workspace directory; discovery associates those whose exact directory
matches the checkout without resetting context. Remote Plank sessions are excluded
because their host controls their execution directory. Daemon launches resolve and
verify the checkout, and metadata probes run per directory. The UI's common directory
resolver and terminal spawn path honor the selected session checkout. Removed checkout
records remain as tombstones; archived transcript views never fall back to the main
directory. Removal guards all workspaces referencing a path, and cannot force Git to
delete dirty or locked trees. A recreated checkout must be explicitly rediscovered.

Discovery now runs automatically on workspace creation and in a background startup
pass for existing workspaces. Tests prove both paths. Startup does not revive removed
checkout tombstones; explicit discovery is required to reattach recreated paths.
Workspaces rooted in repository subdirectories group under the deepest attached containing
checkout without rewriting their session execution directory. A regression covers this,
explicit associations, missing identities, removed checkouts, remote sessions, and path
component boundaries.

Use the repository's canonical Git common directory as local project identity, scoped
to the daemon host. Branch names and remote URLs are display metadata, not identity.
Non-Git directories must remain usable. Worktree discovery and mutations belong on the
daemon, not in GPUI render callbacks. Preserve existing sessions through migration.

The checkout already contains substantial uncommitted implementation work. Preserve
it; do not reset or replace these files from HEAD.

## Latest keyboard and native QA findings

The 1180-logical-pixel QA window visibly renders a collapsed red attention rail.
Automatic docking now requires at least 1600 logical pixels, covering small laptop
and tablet layouts; Pinned remains available. The test matrix includes 1024, 1440,
1470, 1600, and 1920 widths, plus expanded sidebar widths. Keyboard session switching
exposes native radio and checkbox controls in the accessibility tree. Dynamic screenshot
capture still appears stale and coordinate interactions sometimes return
`noWindowsAvailable`, so full visual/interaction validation is incomplete.

QA fixtures now create a disposable Git repository with a linked checkout and native
single/multi-select question forms. `gpui-toolkit` Checkbox previously had no focus
target despite its key handlers. It now retains focus per window with keyed state,
participates in Tab order, skips disabled controls, and stops propagation after toggling.
A focused keyboard regression was added in the toolkit integration suite. The app
also adds Focus pending question (Cmd/Ctrl-Shift-U), without moving focus on arrival.

Closing the final session tab no longer causes the next snapshot to reopen it.
The latest QA launcher was cleaned up through its own signal handler; the installed
NPlank app was not touched.

The toolkit keyboard regression initially failed (Tab left focus on the first
checkbox). Explicit Tab/Shift-Tab traversal was added to both Checkbox and RadioGroup;
the checkbox focus/redraw regression now passes. A separate regression verifies
radio selection followed by Tab to an explicit confirmation button.

The `--questions` QA launch provides an initial screenshot of native radio controls,
explicit confirmation, bounded questions above the composer, and the compact status
rail. The screenshot also exposes a real remaining gap: gpui-ui-kit Markdown only
parses blocks; inline emphasis markers remain literal in paragraphs. Implement native
inline Markdown styling/links and verify it before claiming the Markdown requirement.
Runtime keyboard-action evidence is still weaker than component integration tests:
AX exposes controls but repeated CUA actions do not consistently show state changes.

Final checks for this pass: 242 app/library/worktree/document tests, 142 headless
tests, and 9 toolkit checkbox/question integration tests pass. Clippy with
`--features test-engine --all-targets -- -D warnings`, rustfmt, and diff whitespace
checks pass (the pre-existing upstream `block` future-incompatibility notice remains).


## Final implementation verification

The inline Markdown gap observed in the earlier screenshot is now fixed in the sibling
`gpui-toolkit`. Native styled text handles nested emphasis, inline code, strikethrough,
and web/mail links. Links support Tab focus, arrow selection, Enter activation, and
native accessibility labels. Ordered lists and basic tables render as native layouts.
This is a practical Markdown subset, not full CommonMark block compatibility (for
example, nested lists and reference-style links are not fully supported).
Nine focused Markdown tests pass, including a GPUI keyboard test proving links open
only on explicit Enter. Nine checkbox/question integration tests passed previously.

Appearance windows now preserve keyboard presets, binding overrides, and diagnostics
when saving appearance changes; other native windows adopt shared keyboard settings.
A persistence regression covers the stale-window case. Session tabs use quiet styling,
confirmation buttons size to their content, and transcript messages have a readable
maximum width.

Final verification: 243 app/library/worktree/document tests pass, toolkit Markdown tests
pass, both packages pass formatting checks, and both diffs pass whitespace checks.
App Clippy with `--features test-engine --all-targets -- -D warnings` passes; the
pre-existing upstream `block` future-incompatibility notice remains. The earlier
headless run passed 142 tests.

The final disposable QA app launched, but computer-use app selection timed out twice.
The launcher and its owned app/daemon were stopped via their cleanup handler. Therefore
the newest Markdown and spacing changes have automated/component verification but no
fresh native screenshot. Earlier screenshots verified the compact rail and native
question layout. Live resize, touch, and exhaustive end-to-end keyboard/worktree flows
remain manual QA items; no iPad-device validation is claimed.


## Composer, top bar, and panes follow-up

The titlebar now distinguishes the New session action from the current session's
label. Fleet, CI, Monitor, and Docs live in the top bar with responsive icon-only
labels. ChatGPT and the user-selected Meta mark identify the corresponding agents;
local Plank has no redundant provider badge. Workspace and machine rows include a
stop/kill action that preserves transcript history.

The shared toolkit Input now supports native multiline layout, wrapping, selection,
mouse hit testing, and vertical cursor movement. Enter submits explicitly; Shift-Enter
inserts a newline and blur never submits. The composer grows to a bounded scroll
area and puts attachment, stop, and context-reset icons above it. Typed slash commands
complete with the keyboard, prioritizing command-name prefixes over help-text matches.

Session-header controls and configurable shortcuts split right/down or close a pane.
Each child has scoped input identities, independent selection/drafts, and focus-aware
shortcut dispatch. Nested splits are supported. Panes use equal proportions and are
not restored after closing the window; draggable dividers are future work.

Verification: 244 app/library/worktree/document tests and 52 toolkit input tests pass.
The new GPUI pane test verifies independent drafts, slash completion, child keyboard
dispatch, and split geometry. The multiline test verifies height growth, Unicode,
Shift-Enter, explicit submission, and no submission on blur. Clippy with test-engine
and all targets, formatting, and whitespace checks pass. The initial native QA
screenshot verified the toolbar/composer layout; later captures remained stale, so
interactive split validation relies on the GPUI test. iPad device QA remains pending.
