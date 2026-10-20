# View policy, elastic leases and atomic replace

Date: 2026-10-20. Status: draft for builder review.

## 1. Intent and why

Today the waterfall page holds the radio. Its own `spectrum` job leases the source at the width of the
view, is hidden from the job list, and re-creates itself as soon as it is stopped, so a wider job (ADS-B
needing 2.4 MHz while the view sits 200 kHz wide at 1090.5 MHz) can only be started by racing it. A
decoder's lease is rigid by design; the page's is not a requirement at all, it is a wish to see whatever
the radio is capturing around the view.

The second driver is a two-device bench: a B210 for the targeted receiver (a decoder or a demodulator on
a chosen band) and a LimeSDR on a shared antenna for wideband scanning. Both chains of one B210 share one
local oscillator, so channels A and B cannot do this; two devices can, and the node already treats each
`--source-*` flag as its own source with its own lease.

A third driver appeared on 2026-10-20 when the node on the field host was restarted for an upgrade. Before
the restart one 10.24 MHz capture held four broadcast demodulators and the page's spectrum job; resume
replayed the persisted jobs one at a time in record order, so the first demodulator leased the radio
200 kHz wide, every other running job on the source failed with `conflict`, and that first job failed
too because its persisted graph had been expanded for the old capture rate (`out_rate_hz 256000 must
divide rate_hz 202105`). Resume must restore the capture the jobs shared, not replay their leases
blindly. And when jobs disappear nobody can say which client stopped them: the `ops` stream is not
persisted.

The user's words for the behaviour: stay on the decoder's viewport first; allow zooming out to observe
the stitched rapid scan of the wider channel, with an overlay of the targeted receiver on top of that
coarser wider view; when the targeted channel's parameters change, gracefully zoom back into the new
region; make this behaviour user-settable later, command-driven for now since there are not enough items
for a settings panel; zooming out starts a scan itself, that is the whole point; scan the zoomed-out
span, capped by the device range; stop the scan when zooming back in, leave manual scans alone, and make
that reconfigurable; the core goal is an extendable framework, more behaviours will come.

## 2. Goals, non-goals, anti-goals

Goals:

- The page never blocks a decoder. Its spectrum job yields to any rigid request and follows the capture.
- Starting a job can hand the radio over atomically: a client names the holders it replaces and nothing
  slips in between.
- The waterfall shows two layers: the targeted capture at full resolution, and, when zoomed out past it,
  a stitched scan from a free device underneath, with the targeted band outlined on top.
- The behaviours (what the view follows, what zooming out does, when an automatic scan stops, which
  device scans) are named policies with defaults, settable by a typed command, and adding a policy is
  adding one table entry.
- A restart brings every running job back the way it was: the capture is restored before the jobs
  that shared it, and a job whose graph was built for another capture rate is re-expanded, never failed.
- Every operation applied to the node is on disk with its caller, so "who stopped that job" has an
  answer after the fact.

Non-goals (later work, not this design): widening a held capture so two rigid jobs coexist (the lease
already names it as "not yet available"); exposing the B210's second chain as a second source; a settings
panel; persisting policies on the node so the agent can set them (the command shape is chosen so that a
node op can mirror it later); installing the Lime driver stack on the field host (operator work).

Anti-goals: a decoder's lease is never retuned by the view; an automatic scan never touches a scan the
page did not start; nothing the page does is hidden from `job_list` any more than it is today, and the
page's own job is marked as what it is.

## 3. Node side

### 3.1 Elastic leases

`job_start` gains `elastic: bool` (default `false`; CLI `--elastic`). In this design only `spectrum`
accepts it (`JobKind::elastic_capable()`; another kind is `OpError::Other("<kind> cannot be elastic")`).
A lease records the flag; `JobRecord` gains `#[serde(default)] elastic: bool` (format stays 1);
`job_list` shows it; `source_list` adds `elastic: [job ids]` beside `holders` (which lists rigid holders
only, so existing clients keep their meaning).

Rules, all under the existing per-source gate:

- A rigid request (`job_start` without `elastic`, `source_ensure`) ignores elastic holders when deciding
  conflicts. If only elastic jobs hold the port, the request tunes it as if unheld.
- After any retune of a port, every elastic job on that source is relaunched on the new capture: same job
  id, new graph expanded for the capture, new stream ids, record `tuning` set to the capture and persisted.
  This reuses the restart path `graph_put` already has; the page already follows stream id changes.
- An elastic request on a held port joins the current capture (its `tuning` becomes the capture's), never
  retunes it, and never conflicts.
- A time-shared lease (a `scan`) on the source stops the elastic jobs there (`Stopped`); the page handles
  scan mode as it does today and starts a fresh elastic job when the scan ends.

### 3.2 Atomic replace

`job_start` and `source_ensure` gain `replace: [JobId]` (default empty; CLI `--replace ID[,ID]`). Under
the source's gate the node checks every named job holds a lease on that source (`unknown_job`, or
`OpError::Other("job <id> does not hold source <src>")`), stops them with full `job_stop` semantics
(persisted `Stopped`, streams unregistered), then proceeds with the request. Replace is not transactional
across a later failure: if the request still conflicts with a holder that was not named, the replaced
jobs stay stopped and the `conflict` reply lists the remaining holders. The `conflict` reply's `options`
gain a line `retry with replace: [<holders>]` and keep the existing ones. Elastic jobs never need
replacing and are not listed as holders.

The guide gets a note on both fields; `AGENTS.md` and the skill are regenerated; the schema test covers
the new fields.

### 3.3 Resume restores the capture

At `Node::open` with `resume`, the running records of one source are launched widest band first (ties by
`created_ns`), elastic records after every rigid one, and attached records after their inputs (as today).
The first launch tunes the source to the widest band, so the narrower jobs that shared that capture fit
inside it again instead of conflicting; a job that still conflicts fails with the `conflict` reason as
today (it never fitted the capture it was persisted under, or that capture's job is gone).

Before launching a resumed record of a built-in kind, the node compares the capture rate the record's
graph was expanded for with the source's actual rate after the tune; when they differ it re-expands the
graph from the record's `kind` and parameters for the actual rate (`expand` already takes the source
rate) and persists the new graph under the same job id and stream names. A graph the operator changed
with `graph_put` is not re-expanded (the node cannot know its intent); it is launched as stored and
fails with its own reason if the rate no longer fits. To know what a graph was expanded for,
`JobRecord` gains `#[serde(default)] capture_rate_hz: Option<u64>` (format stays 1; records without it
are treated as matching, which is today's behaviour).

Tests (core, file sources): two records of different widths on one source resume in either on-disk
order and both run; a record whose graph was expanded for one rate resumes on a source at another rate
and runs with a re-expanded graph; a `graph_put` graph is left alone; an elastic record resumes after
the rigid ones and follows their capture.

### 3.4 The ops log

Every operation the node applies (the records already on the `ops` stream: op, result or error,
transport, `t_ns`) is appended as one JSON line to `<state_dir>/ops.log`, rotated to `ops.log.1` at
8 MiB (two files at most). `zeta ops tail [--lines N]` prints the last N lines (default 50) from the
state directory (one-shot, local; `--node` is a usage error), so an operator can see which client
stopped a job after the fact. The log is written by the node, never read by it; nothing else changes.

Test: a `job_start` then `job_stop` over `/op` with `X-Zeta-Client: ui` leave two lines whose
`transport` is `ui`; rotation at the size limit; `zeta ops tail` prints them.

## 4. Page side

### 4.1 The page's job

The page starts its own spectrum job with `elastic: true`. While a rigid job holds the shown source the
page never retunes: the view is a window onto the capture, zooming and panning inside it are local, and
leaving it is handled by the coarse layer (4.3). When nothing rigid holds the source, leaving the capture
retunes through `source_ensure` as today while the view still fits the shown source's `max_bandwidth_hz`,
and the node's relaunch of the elastic job replaces the page's stop-and-restart dance; a view wider than
that is the coarse layer's job (4.3, 4.4), exactly as when a rigid job holds the source.

### 4.2 Layers

- Fine layer: the rows of the followed capture (the page's elastic job, which by 3.1 always equals the
  capture of the rigid holder when there is one). Drawn as today.
- Coarse layer: the rows of a `scan` job on the scanner source (4.4), stitched per step as scan mode
  draws them today, painted across the whole view underneath the fine layer.
- Overlay: the fine layer is painted over its own band on top of the coarse rows, and every rigid
  holder's band (each running rigid job's `tuning` on the shown source, from `job_list`; several jobs
  may share one capture, and a job's band may be narrower than it) is outlined and labelled with its job
  kind. With no rigid holder the outline is the capture itself. "The targeted capture" that the view
  follows is always the port's current capture (`source_list` `current`), not one job's band.

### 4.3 Policies

One registry object in the page, each entry `{key, values, default, apply(ctx)}`, state kept in
`sessionStorage` with the other page state. The command parser and `view show` are generic over the
table, so a new behaviour is a new entry.

| key | values | default | meaning |
|---|---|---|---|
| `follow` | `target`, `free` | `target` | `target`: the view tracks the shown source's capture, and when that capture changes (`source_list` `current` differs from the last poll) the view eases to the new region over about 400 ms instead of jumping; `free`: the view never moves on its own |
| `zoom_out` | `scan`, `none` | `scan` | `scan`: zooming or panning past the fine capture starts an automatic scan over the visible span on the scanner, re-issued (debounced, about 300 ms after the last change) as the view grows or moves, capped to the scanner's tune range; `none`: the coarse layer stays empty |
| `auto_scan_stop` | `inside`, `keep` | `inside` | `inside`: the page stops its automatic scan when the view returns inside the fine capture; `keep`: it keeps sweeping |
| `scanner` | `auto`, a source id | `auto` | which source scans: `auto` picks the first source that is not the shown one, has no rigid holder, and whose tune range contains the view's centre; with no such source it falls back to the shown source itself when nothing rigid holds it (today's scan mode: the scan stops the page's elastic job, which restarts when the scan ends); a source id pins it |

Commands, typed in the command line: `view set <key> <value>`, `view show`, `view reset`. The page
handles text starting with `view ` before the op and `/ask` paths; an unknown key or value is a
transcript error naming the allowed values. The shape (`key`, `value`) is what a later node op
`view_set` will carry, so the agent can set them without a redesign.

### 4.4 Automatic scans

The page remembers the id of the scan it started (`sessionStorage`), never by kind: a scan found running
on the scanner that the page did not start is manual, is displayed in the coarse layer, and is never
stopped or retuned by the page. When no source can scan (the only candidates are held by rigid jobs),
zooming out shows an empty coarse layer and one transcript line saying why. An automatic scan is started with `job_start kind=scan`
over the visible span (capped to the device's range), and replaced by a new one when the span grows or
moves (stop the old, start the new; the brief gap is acceptable).

### 4.5 Visible state

The page's own elastic job appears in the job list like any job, marked `elastic` (the current hiding
goes away; the row needs no buttons beyond stop). The scan row shows whether it is the page's own.

## 5. Agent and CLI

`zeta job start --elastic`, `zeta job start --replace ID[,ID]`, `zeta source ensure --replace ...`. Guide
wording: when a `conflict` names holders, decide whether those jobs may stop, then retry with `replace`;
the page's elastic job is under `elastic`, never a holder, and needs no replacing.

## 6. Constraints

- `zeta-core` gains no dependency; records keep format 1 with defaulted new fields (`elastic`,
  `capture_rate_hz`); the ws and plugin wires are unchanged.
- The page already reloads itself when the node's `build` (from `/health`) changes or its uptime goes
  backwards (`main` at f7757cd), so a deployment of this work needs no manual reload of open pages.
- `stream tail --timeout-ms` is capped at 10 000 by the node; scenarios that wait longer loop.
- Gates as plan 10: fmt, clippy `-D warnings`, the crate tests, the Playwright UI suite with no skip line,
  the guide regenerated and checked.
- Tests need a second source as the scanner. File sources step a scan today (the core scan tests use a
  fixture); the builder confirms a looping file source can serve as the scanner in the UI suite, and if not,
  adds the smallest fixture-source support that makes it so.
- Secrets and flags unchanged.

## 7. Left to the builder

The easing curve and exact durations; the debounce; how the elastic relaunch shares code with the
`graph_put` restart; the schema shapes for `elastic`, `replace` and the `source_list` field; the overlay's
drawing order and outline style; how the resume order and re-expansion share code with `Node::open`'s
existing attached-after-input ordering; the ops log writer's place in `apply_from`; the Playwright
scenarios (at least: a rigid start over the page's job
succeeds without replace; replace hands over atomically against a competing start; zoom out starts a scan
on the second source and zoom in stops it; a manual scan is left alone; `view set follow free` holds the
view across a retune; the retune eases under `follow target`).

## 8. Calls made in this document

- Elastic is allowed on `spectrum` only; other kinds are refused.
- A scan on the source stops its elastic jobs rather than relaunching them per step.
- Replace is not transactional across a later conflict.
- Policies live in the page (`sessionStorage`) now; the node op is a later mirror.
- An automatic scan is replaced, not retuned, when the span changes.
- With no rigid holder, zooming out retunes the shown source while the view fits its maximum capture and
  scans beyond that; so single-source nodes get the coarse layer too, on the shown source itself.
- The overlay outlines every rigid holder's band (a list), and the view follows the capture, not one job.
- `follow` has no `scan` value: an automatic scan's span is derived from the view, so tracking it would be
  circular; a manual scan is displayed in the coarse layer and the view stays where the user put it.
- Resume orders a source's records widest first and re-expands built-in graphs for the actual rate; a
  `graph_put` graph is launched as stored. The capture rate is persisted per record rather than per
  source, so a record carries what it needs on its own.
- The ops log is a plain file with a one-shot reader, not a new op or stream: the smallest thing that
  answers "who stopped that job".
