# Monitoring: Prometheus dashboards with gpui-d3rs

A Grafana-style monitoring view for the current workspace, next to Fleet,
CI, and Docs (sidebar `Monitor`, View → Monitoring, `Cmd+Shift+G`).

## Source

One global Prometheus-compatible source, set at the top of the Monitor
panel and stored in Preferences (`prometheus_url`, default
`http://localhost:9090`). Fetches go through the system `curl` (same
thin-process pattern as the CI board; 15s timeout) against the standard
HTTP API — `/api/v1/query_range`, `/api/v1/label/__name__/values`,
`/api/v1/series` — so Prometheus, and Prometheus-compatible stores that
serve the same API, work unchanged. All parsing in `src/prom.rs` is pure
and tested: matrix and vector replies, `NaN`/`±Inf` points dropped,
Prometheus error statuses surfaced verbatim.

## Dashboards (`src/dashboards.rs`)

Dashboards are JSON files in `<workspace>/.nplank/dashboards/<slug>.json`
— versionable in git, shareable, hand-editable. A dashboard is a name
plus up to 24 panels `{title, kind, query, unit}`. The UI can create and
delete dashboards (delete asks for confirmation, like closing a
workspace), add/edit/delete panels, and seed a starter dashboard
(`up`, `scrape_duration_seconds`). Invalid files on disk are skipped by
the lister, never deleted, so a hand-edit typo costs nothing.

Panel kinds: **Line** and **Area** (multi-series time series), **Bar**
(latest value per series), **Stat** (latest value plus change over the
range).

## Building panels

The metric browser lists metric names from the source, searchable;
clicking one starts a new panel prefilled with that metric (or replaces
the PromQL of the panel being edited) and fetches its label keys as a
hint for filters. The PromQL field is then fully editable — the browser
is a starting point, not a cage. Ranges: 15m / 1h / 6h / 24h; step targets
~120 points per series. Panels auto-refresh every 30s while the Monitor
view is visible, plus a manual Refresh.

## Rendering with gpui-d3rs

Charts are drawn with `gpui-d3rs` (the `d3rs` crate from gpui-toolkit):
`LinearScale` for both axes, `Scale::ticks` for gridlines,
`Curve::monotone_x` interpolation (no overshoot on spikes — the right
default for metrics), and the category10 `ColorScheme` for series colors.
GPUI paints the resulting paths on a `canvas`; min…max extremes and
colored legends sit under each chart. Bar/Stat use the same colors and
value formatting (`1.23M`, `21.5`, …).

## Verification

- `cargo test --features test-engine`: all targets pass (225 lib tests;
  `prom` parsing/URL/step tests and `dashboards` slug/roundtrip/
  validation tests included).
- `cargo fmt --check` and
  `cargo clippy --all-targets --features test-engine -- -D warnings` pass.
- The fetch contract was verified live against a fake Prometheus-shaped
  HTTP server using the exact `curl -sS -g --max-time 15` invocation and
  URLs the code builds (matrix reply with string-encoded values parsed
  back correctly).
- Live-verified end to end in a disposable QA app (own daemon/workspace,
  pid-targeted real HID events, every frame screenshot-read): Monitor
  opened via sidebar click and `Cmd+Shift+G`; starter dashboard created
  (file on disk checked); Line chart, Stat, Area (filled) and Bar panels
  rendered from the fake source; metric browser list, search filtering,
  metric click prefilling the editor with title + PromQL, and label keys
  (`instance, job`) shown; panel saved to JSON, edited (kind cycled
  Line→Area→Bar), and deleted; a second dashboard created, deleted via
  the confirmation modal (file removed); dashboards reloaded from disk
  after an app relaunch; ranges 15m/1h/24h refetched with visibly
  different data; 30s auto-refresh proven by the source request log
  (4 new `query_range` requests in 35s, start timestamps exactly 30s
  apart, zero clicks in between).
- Also verified against a real Prometheus at `192.168.1.32:9090`
  (1907 metrics): URL applied and persisted to Preferences, real
  multi-series Stat/Line panels rendered, and a hand-written PromQL
  panel (`avg by (job,mode) (rate(node_cpu_seconds_total[1m]))`)
  added through the editor and rendered.
- One visual bug found and fixed during verification: Bar panels used a
  fixed 360px bar that overflowed the card; the bar is now a fraction of
  its track (`gpui::relative`).
- Not verified: View → Monitoring via a synthetic AX menu press returned
  success but did not toggle (same synthetic-input limitation seen with
  other GPUI menus here); the menu item shares the `ToggleMonitor`
  action with the verified sidebar button and shortcut. No FPS tool was
  available; interactions responded within the 1–3s observation cadence
  and the top-bar CPU stayed at 12–28% while charts rendered.
