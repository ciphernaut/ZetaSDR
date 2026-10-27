# Mode registry and external-decoder adaptor: breadth for layer 2

Date: 2026-10-27. Status: draft for builder review. Plan 16 in the sprint order
(`docs/superpowers/plans/2026-10-18-zetasdr-plan-series.md`, "Sprint 2026-10-27").

## 1. Intent and why

The design (`2026-10-18-zetasdr-design.md`, section 6) classifies a signal in four layers: layer 1
measures the modulation, layer 2 names a known signal type by signature match or by
decode-to-identify (a decoder runs and a CRC-clean frame is the proof), layer 3 is a trained
model, parked, and layer 4 is the agentic loop that works an unknown signal and writes back a
signature and a labelled capture. The operator's wish for a new signal on the waterfall is that
flow, in that order, with as little guesswork as possible.

Layer 2 can only prove what it has a decoder for. The node has five (`aprs_decode`,
`pocsag_decode`, `rds_decode`, `acars_decode`, `adsb_decode`) against the 594 signatures of the
iNTERCEPT catalogue it already carries and the 142 modes OnAir ships. Every decoder today is a
variant of the closed `JobKind` enum with its graph written by hand in one `match`
(`crates/zeta-core/src/ops.rs`, `JobKind::expand`), and the one external decoder, dump1090, has a
700-line bespoke wrapper (`crates/zeta-ext/src/adsb.rs`). A hundred modes cannot arrive that way.

This plan makes a mode a data row the node loads, validates and tests, gives external decoder
programs one declarative adaptor, and lets the catalogue's score order the decode sweep so that a
box on the waterfall gets a quick ranked answer and an `identify` job works through everything
plausible. It ships no new mode: `adsb_decode` is re-expressed as the first row to prove the
shape. Plan 17 brings the modes, plan 19 the layer-4 stages that take over when `identify` ends
in `unknown`.

Decisions in this document were made in the brainstorming conversation of 2026-10-27 unless
marked "builder's call" (left open) or "author's call" (my choice, open to change).

## 2. Goals, non-goals, anti-goals

### Goals

- A mode is a row: id, family, default band, decoder (an in-crate graph template or an external
  program), event shape, decode-to-identify hook, catalogue link, and its own test. Rows are data
  files; a default set is embedded in the binary and an operator directory layers over it.
- Every row carries proof: a generated test signal (or a fixture) and the event it must decode
  to. `zeta modes check` runs it; CI runs it over every embedded row.
- A bad row or a missing external program never stops the node. Both are logged at startup and
  listed with their reason; the mode is marked `broken` or `unavailable`.
- Missing programs are reported OpenWebRX-style: one line per handler, found with version or
  missing with a suggested host package where one exists, otherwise the project link. zeta never
  fetches or builds a handler.
- One external-decoder adaptor replaces the dump1090 wrapper: a program, its arguments, the input
  it takes (IQ or audio, in a stated sample format and rate, on stdin or a pipe) and the parser
  for what it prints (line JSON, SBS, NMEA, KISS, plain text), all declared in the row.
- A common decoded-event envelope every mode fills, with a one-line `summary` the page draws
  over the waterfall and the agent reads, so neither needs to know the protocol.
- Two sweeps: `classify` tries the top few modes in catalogue-score order on its one-shot span
  and reports what it tried; a new `identify` job kind works through every plausible mode with
  a proper dwell each, publishing a `candidate` event per attempt, and ends with a verdict or
  `unknown`.
- Existing job kinds keep their names and behaviour; today's clients and scripts keep working.
- The stream payload envelopes for `video`, `text` and bits are settled (shape and limits), so
  plan 17's image modes and plan 19's layer-4 stages have somewhere to land.

### Non-goals (plan 16)

- No new decoder mode. `adsb_decode` as a row is the proof; the first wave is plan 17.
- No feeds out (APRS-IS, SBS/Beast, AIS NMEA, PSK Reporter, WSPRnet, KISS). Plan 17, with the
  modes that need them.
- No model work: no layer-3 plugin, no TorchSig training. A training-free LLM classifier is its
  own small plan after this one; TorchSig training waits for a second GPU (section 3).
- No layer-4 stages. `identify` ending in `unknown` is the hook; plan 19 fills it.
- No re-implementation of small external decoders in Rust. Direction only: the row format must
  not care whether a decoder is a process or a stage, so a later sprint can swap one for the
  other and the row's test proves them equal.
- No UI redesign. The page gains only what the envelope's `summary` gives it for free and a way
  to start `identify` from a box; mode picking by band and the Okudagram theme are plan 18.
- No span decoders (every POCSAG channel in a capture at once) and no trunking followers.

### Anti-goals (what would make this a failure even if it worked)

- A mode that is listed but silently untested. Every shipped row has a passing check or is
  marked `broken` where the operator can see it.
- A node that refuses to start because of a data file or a missing program.
- A sweep that says `unknown` without saying what it tried.
- A `summary` line the page renders that hides the fields the agent needs. The envelope keeps
  both.
- A registry that makes today's `pocsag_decode --then-tail` or the UI's decode menu stop working.

## 3. Constraints

- Rust 2024 workspace; no new crate dependency in `zeta-core` (the row schema and registry live
  there, as `plans.rs` does). TOML parsing, if chosen, may use the `toml` crate in the crate that
  loads files (author's call: `zeta-core` embeds rows as JSON strings generated at build time
  from TOML sources, or the rows are JSON throughout; builder's call which, under the rule that
  an operator edits one file per mode).
- Project rules (AGENTS.md): one unit per field (`_hz`, `_baud`, `_ns`, `_ms`, `_db`), every
  field required, unknown fields rejected, errors carry `field` and the acceptable range, ids
  never invented. A row's schema follows the same rules; its validation errors read like op
  errors.
- Op results are additive; nothing existing changes shape. `AGENTS.md` and `SKILL.md` are
  regenerated from the schema, so every new op and every note gets its generated text.
- The node must work offline. Suggested packages and project links in the handler report are
  text in the row, not a network lookup.
- External programs run the way plugins do: a cleared environment (`PATH`, `HOME`, `LANG`,
  `LC_ALL`, `LC_CTYPE`, `TERM`, `PYTHONPATH`, plus `ZETA_PLUGIN_ENV_*`), stderr copied to the
  node's stderr with a prefix, a probe at startup, and a crash that fails the job naming the
  program.
- Hardware: the field host's RTX 4090 runs Gemma 4 for speech and the agentic loop; it is the node's
  inference box. An RTX 3060 (12 GB) is not yet on the network. Nothing in plan 16 adds GPU load;
  any later layer-3 training must not displace the node's inference (it waits for the 3060).
- Time: UTC-slotted modes (FT8, WSPR) and TDMA modes need true block timestamps. Plan 14's clock
  guard is the precondition; its live acceptance is still pending, so plan 16 may define rows
  for such modes' tests only with generated signals, never claim them live.
- The UI suite runs near its 300 s deadline (about 238 s). Plan 16 adds at most one scenario.

## 4. The registry

### 4.1 A row

One file per mode. Fields (all required unless marked optional; names follow the unit rule):

- `id`: the mode id, lowercase snake case, the string `job_start --kind` takes. Existing kinds
  keep theirs (`pocsag_decode`, `adsb_decode`, ...).
- `name`: a short human name ("POCSAG pagers").
- `family`: one of a fixed list taken from OnAir's grouping, author's call: `tv`, `broadcast`,
  `aviation`, `maritime`, `satellite`, `two_way`, `cellular_rail`, `pagers`, `iot`,
  `weather_science`, `amateur_digital`, `amateur_voice`, `hf_utility`, `general`.
- `band`: `{ "bandwidth_hz": N, "centers_hz": [..] }`, the channel width the decoder expects and
  the well-known centres (ACARS's 131 550 000 and friends; empty for "anywhere").
- `modulation`: the layer-1 label(s) this mode presents as (`fm`, `fsk`, `am`, `wfm`, `ssb`,
  `carrier`), used by the sweep's scoring.
- `decoder`: one of
  - `{ "kind": "graph", "nodes": [...], "edges": [...] }`: a graph template in today's
    `Graph` shape, with the channel node's `center_hz`/`bandwidth_hz`/`out_rate_hz` written
    `"$center_hz"`, `"$bandwidth_hz"`, `"$out_rate_hz"` for the expander to fill (author's call
    on the placeholder spelling; builder's call if the existing `expand` wants something else);
  - `{ "kind": "program", ... }`: an external decoder, section 5.
- `event`: the mode's event payload schema (JSON Schema, the subset `schemars` emits), and
  `summary`: a template for the one-line text, e.g. `"{address}: {text}"`, with `{field}`
  substitution and `?` for an optional field that is omitted with its separator when null
  (author's call; the page's `protocolText` in `overlays.js` shows the lines each existing mode
  prints today, and they must come out the same).
- `label`: the layer-2 label name a proof produces (`APRS`, `POCSAG`, `ACARS`, `RDS` for the
  existing rows, pinned by test so the page's `menu.js`/`agent.js`, the agent and scripts keep
  reading the names they read today; author's call after the builder check).
- `identify`: the decode-to-identify rule: `{ "proof": "event" | "field", "field": "fcs_ok",
  "min_events": 1, "dwell_ms": 3000, "min_span_ms": 1000, "quick": "aprs" | null }`. `event`
  means any event is proof (CRC-checked decoders emit only clean frames); `field` names a
  boolean field that must be true. `dwell_ms` is how long the `identify` job waits for proof.
  `min_span_ms` is the shortest selection `classify` will try the row on (author's call: 1000,
  today's one second). `quick` names one of a fixed set of in-crate attempt functions (today's
  `aprs`, `pocsag`, `acars`, `rds` in `crates/zeta-dsp/src/identify.rs`) that `classify` can run
  on a buffered span without the job runtime; a row with `quick: null` (every `program` row,
  `adsb_decode` included) is tried only by the `identify` job, never by `classify`. That keeps
  today's rule that `classify` never attempts ADS-B.
- `catalogue`: the iNTERCEPT entry ids this mode proves (`decoder_for` in
  `crates/zeta-dsp/src/catalogue.rs` maps the other way today and is replaced by this).
- `test`: section 4.3.
- `notes`: optional free text the generated AGENTS.md carries for the mode (where to find it,
  what band to give it), as the hand-written notes do today.

### 4.2 Loading, layering, validation

- The embedded set lives in the repo under `modes/` (one file each) and is compiled into the
  binary. An operator directory, `--modes-dir PATH` (default `$XDG_CONFIG_HOME/zetasdr/modes`,
  else `~/.config/zetasdr/modes`), is read after it; a file with an embedded row's `id` replaces
  that row.
- Every row is validated at load against the schema in `zeta-core`: unknown fields, missing
  fields, unit suffixes, a graph template that does not type-check against the stage registry,
  an `event.summary` that names a field the event schema lacks, a `program` row naming no
  parser. A failing row is kept as `broken` with `{ "file", "field", "error" }` and logged once
  at startup as `zeta: mode <id> (<file>): <field>: <error>`.
- A `program` row whose program is not found is kept as `unavailable` with the handler report
  line (section 5.3) and logged once.
- Nothing here stops the node. One-shot commands load the registry the same way `--plugin`
  programs are probed today (so a one-shot `source list` prints the same report lines).

### 4.3 The row's test

`test` is one of:

- `{ "kind": "synth", ... }`: parameters for the registry's synthesiser, section 6: the
  modulation, rate, payload and the parameters that mode needs (deviation, sync word, baud).
- `{ "kind": "fixture", "path": "..." }`: a SigMF pair under the repo's fixtures (embedded rows)
  or an absolute path (operator rows), for modes nobody can synthesise yet.

plus `expect`: the event (or the subset of its fields) the decoder must emit, and
`within_ms`. `zeta modes check [ID]` (local only) runs each row: generate or load the signal,
run the row's decoder graph on a file source over it, compare the first matching event, print
`ok`/`fail`/`skipped` per row with the diff, exit 0 only if no checked row failed. A `broken`
row prints its reason and counts as failed. An `unavailable` row (handler missing on this host)
prints `skipped: <handler> missing` and does not fail the run unless `--strict` is given, so the
check is honest on a host without the program rather than red for ever (author's call after the
builder check). CI runs `zeta modes check` non-strict over the embedded set as a test in
`crates/zeta/tests`, and separately proves the adaptor's plumbing (spawn, input conversion, each
parser, exit handling) with the fake program `crates/zeta-ext/tests/fake_dump1090` and fake
line emitters per parser. A `program` row's real decode is therefore proved only by `--strict` on
a host that has the handler (live acceptance, section 10), and `modes list` says so: an
`unavailable` row's `reason` names the handler; a `ready` program row's `checked` field records
when `--strict` last passed on this host, `null` if never (author's call).

### 4.4 Ops

- `modes_list {}` → `{ "modes": [{ id, name, family, band, modulation, state, reason,
  decoder_kind, program }] }`, `state` one of `ready`, `broken`, `unavailable`. CLI
  `zeta modes list [--family F] [--state S]`.
- `modes_show { id }` → the row as loaded, plus its `state` and `reason`. CLI `zeta modes show`.
- `zeta modes check [ID]`: local only, section 4.3.
- `job_start.kind` accepts any `ready` mode id. `schema` lists the registry's ids as the
  `kind` enumeration (so AGENTS.md's `JobKind` line is generated from the registry).
  `knowledge_lookup` signatures' `decoder` names a mode id.

### 4.5 Compatibility

Every existing in-crate decoder kind (`aprs_decode`, `pocsag_decode`, `rds_decode`,
`acars_decode`) becomes an embedded row with the same id and a graph template that expands to
byte-for-byte today's graph (pinned by a test per kind, as plan 13 pinned the off-plan graphs).
`adsb_decode` is the exception and the proof: its row is a `program` row (section 5.1), so its
new graph is `channel` at 2 400 000 Hz out → `external_decode`; its job behaviour (`--then-tail`
events, the failure reason naming dump1090, the 2 000 000 Hz minimum band) is pinned instead of
its graph. The `adsb_decode` stage kind stays registered as an alias that runs the row's
`external_decode` with the row's block, so persisted `job.json` graphs from earlier nodes resume
and `graph_put` edits that name it still load; `ZETA_DUMP1090` keeps working as the first entry
of that row's `programs` list, and the general form `ZETA_HANDLER_<PROGRAM>` (upper-cased, other
characters `_`) does the same for any row, so tests and `node_setup.rs` keep their hook
(author's call after the builder check). `fm_demod`, `wfm_demod`, `am_demod`, `ssb_demod`, `spectrum`, `measure`, `scan`,
`passthrough` and `transcribe` are not modes and stay in `JobKind`; author's call: `JobKind`
becomes `Builtin(..) | Mode(id)` or the registry sits beside it, builder's call which, under the
rule that `job_start --kind pocsag_decode` behaves exactly as today and `job_list` records are
unchanged.

## 5. The external-decoder adaptor

### 5.1 The row's `program` block

```json
{
  "kind": "program",
  "programs": ["dump1090", "dump1090-fa", "dump1090-mutability", "readsb"],
  "args": ["--ifile", "-", "--iformat", "SC16", "--quiet", "--net", "--net-sbs-port", "$port"],
  "input": { "stream": "iq", "format": "cs16", "rate_hz": 2400000, "via": "stdin" },
  "output": { "via": "tcp", "port": "$port", "parser": "sbs" },
  "install": { "debian": "dump1090-fa", "url": "https://github.com/flightaware/dump1090" }
}
```

- `programs`: names tried on `PATH` in order (the first found is used; `zeta` also honours an
  absolute path in the operator row).
- `args`: with `$port` (a free loopback TCP port zeta picks) and `$rate_hz`, `$center_hz`
  substituted.
- `input`: `stream` `iq` or `audio`; `format` one of `cu8`, `cs8`, `cs16`, `cf32` for IQ, `s16`
  or `f32` for audio; `rate_hz` the rate the program expects (the channel node resamples to it);
  `via` `stdin` or `fifo` (a named pipe in the job's temp directory, for programs that insist on
  a file name).
- `output`: `via` `stdout` or `tcp`; `parser` one of `json_lines` (one JSON object per line,
  taken as the event), `sbs` (BaseStation CSV, as dump1090 prints; the current `sbs.rs` becomes
  this parser), `nmea` (AIS sentences), `kiss` (TNC frames, AX.25 decoded to `src`/`dst`/`info`
  like `aprs_decode`), `text` (one event per line, `{ "text": line }`), `multimon` (multimon-ng's
  `POCSAG512: Address: ... Function: ... Alpha: ...` lines; author's call, since it is the one
  text format several planned rows share).
- `install`: the handler report's suggestion: `debian` (and optionally `fedora`, `brew`) package
  names, and `url`.

### 5.2 Running

The adaptor is one stage kind, `external_decode`, whose params are `{ "mode": "<row id>" }`
only: the program block is resolved from the loaded registry at launch, never taken inline from
a graph, so `graph_put` over `--node`, HTTP or the `ask` transport cannot run an arbitrary program
with arbitrary arguments; a program arrives only through a row file on the node's host, the way
a plugin arrives only through a startup flag (author's call after the builder check). `graph_get`
shows the reference, `modes_show` the block. The stage spawns with the cleared environment, feed the input stream converted to the stated format and
rate, read the output through the parser, and publish one `event` per parsed record wrapped in
the envelope (section 8). A program that exits fails the stage naming it and its exit status
(as `adsb_decode` does); one that prints nothing is not an error. The `AbortOnDrop` child
handling and the `child_exit_fails_the_stage` bound from `zeta-ext` carry over. The adaptor
honours the plugin protocol's cleared environment but is not the plugin protocol: the plugin
envelope (`crates/zeta-plugin/src/wire.rs`) is for programs written for zeta; the adaptor is for
programs that know nothing about it.

### 5.3 The handler report

At startup, after the plugin probes, one line per distinct `programs` list across the loaded
rows, in the OpenWebRX style:

```
zeta: handlers: dump1090: found /usr/bin/dump1090-fa (dump1090-fa 9.0)
zeta: handlers: multimon-ng: missing (apt install multimon-ng; https://github.com/EliasOenal/multimon-ng) — modes: pocsag_decode_ext, flex_decode
zeta: handlers: dsd-fme: missing (https://github.com/lwvmobile/dsd-fme) — modes: dmr_decode, p25_decode
```

Version comes from running the program with `--version` or `-V`, bounded to 2 s, text only
(author's call: no version check is enforced). `modes_list` carries the same per row.

## 6. The synthesiser

`crates/zeta-dsp/src/fixtures.rs` already generates APRS, POCSAG, ACARS and tone fixtures for
tests. It becomes the registry's synthesiser with a typed parameter set per modulation family:
`fsk` (rate, tone spacing or deviation, bits), `afsk` (as APRS), `nbfm` (deviation, audio),
`psk` (order, rate, bits), `ook` (rate, bits), each with the impairments OnAir applies: carrier
offset, noise at a stated SNR, a clock error in ppm, and an optional echo. A `synth` test block
names the family and its parameters and a payload; the synthesiser's output is a SigMF pair in
the job's temp directory the check runs the decoder over. Builder's call on the module layout;
the rule is that a new embedded row in plan 17 should need a synthesiser change only for a new
modulation family, not a new mode.

## 7. The two sweeps

### 7.1 Scoring

`catalogue_labels` (`crates/zeta-dsp/src/catalogue.rs`) scores a catalogue entry 0.4 for the
measured centre in one of its ranges, 0.3 for a matching bandwidth, 0.3 for a matching layer-1
modulation. The registry scores a mode the same way from its row (`band.centers_hz` and
`band.bandwidth_hz` against the measured band, `modulation` against the layer-1 label), plus the
score of any catalogue entry it proves. Author's call: the order is by that score, ties broken by
`family` then `id`. A row whose `band.bandwidth_hz` exceeds the measured selection or the
`identify` job's band is skipped with outcome `skipped_band` (no retune: the operator boxed what
they want, and RDS on a 12.5 kHz box or ADS-B on anything under 2 MHz is not plausible); a row
with `quick: null` or `min_span_ms` over the selection's length is skipped by `classify` with
`skipped_quick`/`skipped_span` (never by `identify`). A row's centre score is granted when
`band.centers_hz` is empty ("anywhere"). `identify` tries every `ready` row that fits the band,
in score order, with no score threshold (author's call: the operator asked for thorough and
reporting as it goes; a cap is one line if runtime bites in plan 17), and each attempt runs at the
`identify` job's own tuning.

### 7.2 `classify`: the quick answer

Unchanged contract (a selection, a one-shot result). Layer 2 tries the top `CLASSIFY_TRIES`
(author's call: 5) `ready` modes in score order on the selection's span, each bounded by the
span's length, and the result gains `tried: [{ mode, score, outcome }]`, `outcome` one of
`proved`, `no_frame`, `skipped_band`, `skipped_span`, `skipped_quick`, `unavailable`. The
layer-2 label a proof produces is the row's `label` (unchanged names for the four existing
rows). A proof labels the signal as today (layer
2, evidence `decode <mode>: <summary>`). The existing four hard-wired attempts become the rows'
attempts.

### 7.3 `identify`: the thorough answer

A new job kind, `identify`, started like a decoder (`job_start --kind identify --source S
--center-hz C --bandwidth-hz B`): it holds a rigid lease on the band, measures it for one second
(layer 1), scores every `ready` mode, and runs them in order, each for its `identify.dwell_ms`,
one at a time (author's call: sequential, since each attempt is a graph on the same channel and a
HackRF-class host should not run twenty decoders at once; builder's call whether to run two
in-crate attempts concurrently when cheap). Per attempt it publishes a `candidate` event:
`{ mode, score, outcome, evidence, summary }`. A proof ends the job `stopped` with
`verdict: { mode, summary, evidence }` in `job_context`, and author's call: starts that mode's
job on the same band under a new id, named in the verdict, so the operator is already receiving
it. Exhausting the list ends the job with `verdict: { mode: null, label: <layer1>, features }`,
the layer-1 record plus burst timing, which is what layer 4 (plan 19) will take over from.
`identify` on a source already held is a `conflict` like any rigid start.

The page starts `identify` from a box (one new entry beside "classify"; its `candidate` events
render as ribbons through the envelope's `summary`, no page code per mode). That is the only UI
change.

## 8. Envelopes

### 8.1 Decoded events

Every mode's event is the mode's own fields plus the envelope, additive on today's payloads so
existing consumers (`transcripts.log`, the page's `protocolText`, `cb_watch.py`) keep working:

- `protocol`: the mode id (the page already switches on `protocol` for `pocsag`, `rds`, `adsb`,
  `acars`; existing values are kept as aliases in the row where they differ from the id,
  author's call).
- `summary`: the one-line text from the row's template.
- `t_ns`: the frame's start, as the provenance span carries it (present in the payload too so a
  log line is self-contained).
- the mode's fields, per its `event` schema.

The page's per-protocol text formatting (`overlays.js` `protocolText`) is replaced by
`summary`, with a test that each existing protocol's line is unchanged.

### 8.2 Stream payloads for later producers

Settled now, produced later:

- `text`: the payload is a string; the frame carries `protocol` and `summary` the same way.
- `video`: `{ "width", "height", "format": "rgb8" | "gray8" | "png", "frame": N }` with the
  body in the plugin wire's data-frame form (`B = width × height × bytes`, or the PNG bytes),
  published as a `video` frame; SSTV, APT and fax emit one frame per image line block or per
  image, builder's call in plan 17. Protocol 1 of the plugin wire refuses `video`; protocol 2
  admits it with this payload.
- bits: an `event` with `protocol: "bits"`, `{ "rate_baud", "bits": "<hex>", "count": N,
  "t_ns" }`, at most 64 KiB per event, for layer 4's slicer and bit viewer (plan 19).

## 9. Left to the builder

- Row file format (TOML sources compiled to embedded JSON, or JSON throughout) under the
  one-file-per-mode rule and the no-new-`zeta-core`-dependency rule.
- Whether `JobKind` grows a `Mode(id)` variant or the registry resolves ids before `expand`;
  the compatibility tests decide.
- The synthesiser's module layout and which impairments the first embedded rows' tests use.
- Placeholder spelling in graph templates, and how `reexpand`'s carried params (plan 13) apply
  to a row's template.
- Whether `identify` runs cheap in-crate attempts concurrently.
- How the handler report finds versions for programs without `--version`.
- The migration note, if any, for persisted `adsb_decode` graphs beyond the stage alias.

## 10. Live acceptance (plan 16)

On the HackRF laptop after plan 14's acceptance, or on the field host with the B210: `zeta modes
list` shows the five existing modes `ready` and any missing handler `unavailable` with its
suggestion; `zeta modes check` passes every embedded row; `job start --kind pocsag_decode
--then-tail` behaves exactly as before; `classify` on an APRS burst lists `tried` with `aprs_decode`
`proved`; `identify` on a UHF CB voice channel ends `unknown` with an `fm` label, and on 148.6375
MHz ends with a POCSAG verdict and a running `pocsag_decode` job; a deliberately broken operator
row appears in `modes list` as `broken` with its field and the node runs regardless.
