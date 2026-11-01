# Plan 23: subjects (one aircraft, one row), and two agent fixes

Design spec, 2026-11-02. The operator answered the design questions and left the build unattended
overnight; a reviewer read this as the builder. Answers it gave are marked as the operator's; the rest
are the author's calls, listed in section 8. Builds on plan 16 (mode rows), plan 17 (the map, `position`
on events) and plan 21 (the agent's gate). The entry it answers is `TODO.md`, "UI: ADS-B (and any
state-stream decoder) on the page".

## 1. Intent and why

An `adsb_decode` job fills the waterfall with dozens of boxes for one aircraft: `7C5309 ×14`,
`7C5309 21000 ×6`, `7C5309 QLK335D ×2`. dump1090 delivers identity, altitude and position in separate
messages, several a second per aircraft, and every event is a ribbon. Nothing can be read. APRS,
ACARS and the sondes have the same shape: a stream of updates about a set of things.

So a decoder's events are about *subjects*. A mode row names the event field identifying the subject.
The page merges each subject's events into one current state and shows that: one row in a table, one
marker and track on the map, and at most one line on the waterfall.

## 2. Goals, non-goals, anti-goals

### Goals

- A mode row can name a subject key and table columns. ADS-B, the sonde rows, APRS and ACARS get them
  tonight; RDS gets a key. No per-protocol code on the page (operator: generic, proved on ADS-B).
- A **Subjects** tab beside the map lists every running job's subjects, one row each (operator).
- A job with a table shows one summary line on the waterfall, such as `adsb 12 aircraft, last QLK335D`,
  not a ribbon per aircraft (operator). A keyed row without that setting folds to one ribbon per
  subject, updated in place.
- A subject dims after 60 s of silence and is dropped after 5 minutes (operator); both are view settings.
- Metric everywhere: metres and km/h, converted on the page from what the row declares (operator).
- The map draws each subject with a position as a marker rotated to its heading where known, a label,
  and a track of its last 100 fixes. A click on a table row highlights it on the map, and the reverse
  (operator).
- The agent may `job_stop` jobs it started in the same run without the allowance (operator).
- The guide tells the model to read `channel_plans` before naming a plan (operator: guide rule only).
- `TODO.md` is tidied: sections fully shipped are removed, and only with evidence.

### Non-goals

- Node-side subject state. The page merges from the event stream; a refresh starts empty and refills
  from live traffic. `TODO.md` gets a gist.
- A broadcast FM channel plan (operator). Changing the `unknown` error text (operator).
- Gating `graph_put` or `job_retune`. Staircase ribbons. The theme. Video.
- Selecting or filtering beyond a sort by column. Exporting the table.

### Anti-goals

- A per-protocol special case in `overlays.js`. `protocolText`'s ADS-B fallback goes.
- An unkeyed row changing behaviour: POCSAG messages stay separate ribbons, since each page is its own
  message, and the existing ribbon scenarios keep passing.
- A wider stop gate than asked. `jobs_stop` and any job from an earlier run stay refused without the
  allowance.

## 3. Constraints

- Unknown fields are rejected in rows, and rows are validated at load; a new field is optional and
  `null` by default, so an operator's existing rows keep loading. A broken `subject` block makes that
  row `broken` with `<field>: <error>`, as for any row. `zeta modes check` stays passing.
- The page's event watchers are capped at `MAX_WATCHED = 4` sockets (`overlays.js`), decoders first.
  Five or more subject-bearing jobs would miss events: the Subjects tab says which jobs are not watched.
- `map.js` tracks per job today (`TRACK_POINTS`, `jobs()`), with `distanceBearing` and `view set home`.
- Leaflet is vendored and tiles are online-only; the e2e aborts tile hosts.
- The e2e's `fake_pos` row (`crates/zeta/tests/ui.rs`) proves positions without a radio, and
  `zeta-ext`'s `fake_lines` prints what `ZETA_PLUGIN_ENV_FAKE_LINES` holds.
- The ui e2e deadline is 330 s, with about 290 s used.
- `agent::gate(op, allow)` is a pure function called from `Agent::call`; results reach the model
  trimmed only for `job_list`.

## 4. The design

### 4.1 The mode row

```json
"subject": {
  "key": "icao",
  "name": "aircraft",
  "waterfall": "summary",
  "columns": [
    {"field": "callsign", "label": "Callsign"},
    {"field": "altitude_ft", "label": "Altitude", "unit": "ft"},
    {"field": "ground_speed_kt", "label": "Speed", "unit": "kt"},
    {"field": "track_deg", "label": "Track", "unit": "deg"}
  ],
  "heading": "track_deg"
}
```

- `key`: required; the event field naming the subject. An event without it is treated as unkeyed.
- `name`: the plural noun for the summary line (`aircraft`, `stations`, `sondes`).
- `waterfall`: `summary` (one line for the job) or `fold` (one ribbon per subject).
- `columns`: the table's columns after the key, possibly empty. `unit` is what the event field holds:
  one of `ft`, `m`, `kt`, `km_h`, `m_s`, `deg`, or absent for plain text or numbers.
- `heading`: optional event field rotating the map marker.

`position` gains an optional `alt_unit` (`m` default, `ft`), so `adsb_decode` can give a `position`
from `altitude_ft`. `modes_show` and `modes_list` carry `subject`. Since identity, altitude and
position arrive in separate messages, a position fix with no altitude in its event is valid, as a
partial `position`; if the core requires otherwise today it is relaxed for rows with a `subject`.

Rows tonight:

| Row | key | name | waterfall | columns |
|---|---|---|---|---|
| `adsb_decode` | `icao` | aircraft | summary | callsign, altitude (ft), speed (kt), track (deg) |
| `rs41_decode`, `dfm_decode`, `m20_decode` | `id` | sondes | fold | altitude (m), vertical speed (m/s) |
| `aprs_decode` | `src` | stations | summary | destination, info |
| `acars_decode` | `reg` | aircraft | summary | flight, label, text |
| `rds_decode` | `pi` | stations | fold | none |

The row's event schema names the true field names; use those.

### 4.2 Merged state (a new page module, `subjects.js`)

A store keyed by (job, key). For each decoded event of a job whose row has a `subject`: merge non-null
fields into that subject's state, count it, and stamp `last_seen` from the page's clock. Add `first_seen`,
`count`, and `position` from the event. The row's `summary` template re-renders from merged state, so
`{icao} {callsign?} {altitude_ft?} {lat?},{lon?}` fills in as messages arrive.

Dimming and dropping run on a 1 s timer: dim past `subject_dim_s` (default 60), drop past
`subject_drop_s` (default 300), both `view set` policies persisted like the others. A stopped job's
subjects go when its row goes. `zetaAudio`-style, the store exposes `window.zetaSubjects` for the e2e.

Rows arrive through `modes_list`, which the page already reads to find positioned jobs.

### 4.3 The Subjects tab

Beside the map, in the same area and using its `view set map` idiom: `view set subjects on|off`, and a
tab or button pair so the map and table switch, side by side on the wide footprint when there is room.
It shows only when a running job's row has `subject` columns, or when the operator turned it on.

One section per job with subjects: `adsb_decode b210, 12 aircraft`. Columns: key, the row's columns,
position (`-27.39, 152.43`), distance and bearing from home when `view set home` is set, last seen as
`12 s`, count. Default sort is last seen, newest first; clicking a header sorts by it. Dimmed subjects
are styled dim. A job not watched because of `MAX_WATCHED` shows `not watched (limit 4)`.

Units convert to metres and km/h and display rounded: `6401 m`, `780 km/h`. `deg` shows as `245°`.
Other values show as sent, text truncated with its full value as a tooltip.

All DOM from data is built with `textContent`. The narrow footprint shows the table full width with
fewer columns: key, altitude if any, last seen.

Clicking a row selects the subject: it highlights in the table, and on the map its marker takes a
`selected` style, with a pan if it is off-screen. Clicking a map marker selects its row and scrolls to
it. Selection clears when the subject drops.

### 4.4 The waterfall

In `overlays.js` `ribbon`, a keyed event no longer always makes a ribbon:

- `waterfall: summary`: one ribbon for the job, updated in place at the job's centre: `<kind> <n> <name>,
  last <label>`, where `<label>` is the most recently heard subject's key, or its first column when set.
  `<name>` as the row gives it.
- `waterfall: fold`: one ribbon per (job, key), its text the row's summary from merged state, with `×N`.
  It holds at the right edge while updates arrive and scrolls off once its subject is silent.
- No `subject`: exactly as today.

Remove `protocolText`'s ADS-B fallback, since the row now drives it.

### 4.5 The map

`map.js` tracks per (job, key) for a keyed row, per job otherwise. A marker is rotated by `heading` when
known, with a label of the key or first column, plus altitude in metres when known. A track holds the
last 100 fixes. A popup names the subject and its table's values. Dim and drop follow the store. The
existing per-job behaviour for an unkeyed positioned row stays as it is.

### 4.6 The agent's own jobs

`Agent` records ids from this run's results: `job_start`, `plan_start` (`started[].job` and
`.attached`), `job_attach`, and, for an `identify` it started, the `verdict.job` it reads back through
`job_context`. `job_stop` of a recorded id passes the gate whatever `allow.stop_jobs` says. An id from
outside the run, `jobs_stop` and a non-empty `replace` stay refused without the allowance.

`gate` keeps its signature for the existing tests or takes the run's set as a parameter; the builder
chooses. The record is per run: a second `/ask` knows nothing of the first's jobs, so a job an earlier
run left is refused.

The guide's `job_stop` note and field craft say so: stop your own measure and identify leftovers when
done, others need the operator. "Fitting a demod to a signal" gains the stop.

### 4.7 The guide rule

`RULES` gains: "Plan ids come from `channel_plans`: read it before naming a plan. A band with no plan
takes one `job_start` per measured station; never invent a plan." Field craft "Channel plans" says the
same, with broadcast FM as the example.

### 4.8 Tidying `TODO.md`

For each section, check it against the code and the plan series. A section whose whole request has
shipped is removed. A partly shipped one is cut to what remains. When unsure, keep it. The ADS-B section
is cut to what this plan leaves. The commit message or report lists every section removed, with the
evidence. Add gists: subject state does not survive a refresh; plan 23's own leftovers.

## 5. Testing

- Rows: `subject` round-trips; a bad `waterfall`, an empty `key` or an unknown `unit` makes the row
  `broken` naming the field. An operator row without `subject` loads as `null`. The built-in rows pass
  `zeta modes check`. `alt_unit: ft` converts in `position`.
- Agent (`FakeLlm`): a job a run started stops with no allowance, for `job_start`, `plan_start` and
  `job_attach` ids, and `identify`'s `verdict.job` after it is read. Another id is refused. `jobs_stop` and
  `replace` are refused. A second run cannot stop the first's job.
- Guide: `agents-md --check`, and the rule's text is present.
- Page (`ui.py`), using a new `fake_subjects` program row over `fake_lines`, whose lines are three
  aircraft in separate identity, altitude and position messages, shaped like dump1090's:
  - three table rows, not dozens, with merged fields in metres and km/h;
  - one summary line on the waterfall, and no per-aircraft ribbon;
  - a `fold` row gives one ribbon per subject with `×N`;
  - an unkeyed row still gives separate ribbons;
  - dimming and dropping, with `view set subject_dim_s` and `subject_drop_s` set short;
  - three map markers with tracks; a table click selects on the map, and the reverse;
  - distance and bearing with `view set home`; a header click sorts;
  - the tab appears only with a subject job.

## 6. Acceptance on the field host (the operator's)

1. `adsb_decode` on a radio with the antenna: the waterfall shows one line, the Subjects tab lists real
   aircraft in metres and km/h, and they appear on the map with tracks.
2. An `rs41_decode` or `aprs_decode` job when traffic is about.
3. Ask the agent to fit a signal: its `measure` job stops without `allow stops`, and yours do not.
4. Ask for FM stations on a band: the agent does not invent a plan.

## 7. Left to the builder

- The exact tab markup and CSS, and the rounded formats beyond those given.
- How the marker is drawn (an SVG or CSS arrow, no new asset fetched).
- The new row's internal shape for the `fake_subjects` test.

## 8. Calls made in this document (check these while reading)

- Merged state lives on the page. A node-side store would survive a refresh and serve the agent, but it
  is a larger design; aircraft re-announce within seconds.
- APRS and ACARS take `summary`, and sondes and RDS take `fold`, by how many subjects a job has.
- POCSAG has no subject, since each page is a separate message.
- ACARS's key is `reg`; an event without one stays unkeyed.
- `identify`'s proved decoder counts as the agent's own only once the run has read `verdict.job`.
