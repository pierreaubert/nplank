# Sidebar organisation

Two sections with one consistent interaction model: **clicking a group
header hides/shows the group**, `+` adds, `✕` closes (with confirmation).

## Workspaces

- One group per workspace (project), accent-tinted, sessions as dense
  two-line rows, drag or `Alt+↑/↓` to reorder.
- Terminals are tools of a workspace: they are listed inside their
  workspace group and spawn with the workspace directory as cwd.
- `+` adds a session; `+ Shell` / `+ plank` add terminal tools.
- `✕` asks for confirmation, then **closes the workspace locally**:
  sessions keep running on the daemon (no daemon-side delete exists) and
  stay reachable under Machines. Closed workspaces get a muted
  `↩ name (closed)` reopen row, and selecting any of their sessions
  (e.g. from Machines) reopens the workspace automatically. The closed
  set persists in preferences.

## Machines

Derived from the snapshot (`src/machines.rs`, unit-tested): local, demo,
and agent-backed sessions group under "This Mac"; each distinct remote
host (`Backend::Remote` URL host) is its own machine. Header click
hides/shows (persisted); a row click selects the session.

## Confirmations

Closing a running terminal (sidebar `✕` or the panel's Close button) and
closing a workspace both open a confirmation modal first; exited
terminals close immediately. Confirmed actions run through
`perform_confirmed` in `src/ui.rs`.

## Fleet miniatures

Fleet cards carry a live miniature strip (session summary + status at
8px, refreshed every snapshot poll), and every open terminal gets a
miniature card rendering its actual grid text (last 6 lines) — clicking
one jumps straight into that terminal.
