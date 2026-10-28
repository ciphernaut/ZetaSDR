# Plan 17: radiosondes as the proving mode for the decoder frameworks

Design spec, 2026-10-28. Brainstormed with the operator; a reviewer read it as the builder before
hand-over. Builds on plan 16 (`docs/superpowers/specs/2026-10-27-mode-registry-design.md`): rows,
the external-decoder adaptor, the synthesiser, `classify`/`identify`, the event envelope. Everything
here is written in those terms.

## 1. Intent and why

The operator lives near Example Suburb, Queensland, with a HackRF at the desk, roof discones, and on the field host
a B210 and a LimeSDR side by side. The signal they most want decoded is the Bureau of Meteorology
radiosonde: Brisbane Airport launches twice a day (author's understanding: the synoptic 00Z and 12Z
releases, about 10:00 and 22:00 local, a flight of two to three hours; Rockhampton's on a good day),
and at 20 km up a sonde is well inside radio range. Sondes are rare, so nobody wants to sit watching
for them: the node should watch the 400-406 MHz band on one radio, notice a sonde when it appears,
point a decoder at it on whatever radio is free, and show the live position so the operator can go
and chase it. SondeHub upload comes later.

Plan 17 was pencilled in as "decoder wave 1" and meant as the frameworks sprint. It still is. The
sonde is the proving mode; every piece built for it is generic and the later wave (voice, AIS, FLEX,
whole-span pagers and APRS, stereo) is rows on top of these frameworks, not new machinery:

1. audio-input program rows (the gap plan 16 left: `decoder.input.stream: audio` is refused today);
2. generic synthesiser families (`fsk`, `psk`, `ook`) and the echo impairment, so a row declares
   its test signal without new Rust;
3. a `watch` job: pin a band, spot new signals, identify them, run the proved decoder for the long
   haul on a free radio or in the watch capture;
4. free-tuner allocation: an idle source on any attached node that covers the band;
5. position events and a map layer on the page, for any row whose events carry a position;
6. a feed sink stage kind, SondeHub first, built last.

## 2. Goals, non-goals, anti-goals

### Goals

- A sonde that appears in 400-406 MHz while a `watch` runs is decoded within a minute of its first
  frame without anyone touching the node, and the page shows where it is.
- RS41 decodes through the rs1729 decoder (`rs41mod`, package `sonde-decoders` on this host,
  https://github.com/rs1729/RS) as a program row that reads demodulated audio. DFM and M20 rows
  (`dfm09mod`, `m20mod`, same package, same JSON) ride along as rows only.
- Every sonde frame is one event with the plan 16 envelope (`protocol`, `summary`, `t_ns`) plus a
  normalised `position`, and the ribbons over the waterfall show the summary line.
- `zeta modes check` proves the RS41 row on a synthesised RS41 frame that the real `rs41mod`
  decodes, with no recording in the repo.
- The frameworks are generic and documented: a later row for AIS or DMR needs no code in `watch`,
  the adaptor, the map or the envelope.
- Plan 16's behaviour is pinned: every existing row, test and page text comes out the same.

### Non-goals (plan 17)

- No in-crate RS41 decoder; the external one is the decoder.
- No voice, AIS, FLEX, span decoders or stereo: the later wave.
- No trunking or control-channel following.
- No landing prediction; SondeHub's predictor does that once upload exists.
- No offline map tiles; with no internet the map draws the track and markers on a blank grid.
- No LoRa/MeshCore decode (field craft rule: report, do not decode).

### Anti-goals

- A watch that retunes the radio by itself: it holds its band like `scan` and `identify` do and
  everything it starts runs inside that capture or on another, free, radio. The page's spectrum
  follows as an elastic job; nothing else is taken.
- A sonde row that works only through the watch: `job start --kind rs41_decode` on a measured
  centre must decode on its own, one-shot `--then-tail` included.
- Position fields guessed from field names: a row says which of its fields are the position, or
  the envelope adds none.
- A map that needs a build step or an internet-loaded script: the page stays the vanilla
  modules it is, Leaflet vendored and served by the node like the other assets.
- Anything that uploads by default: the SondeHub sink is a stage the operator adds, with a
  station callsign and position they give.

## 3. Constraints

- Plan 16's constraints carry over unchanged: the unit rule; every row field required unless marked
  optional and unknown fields rejected; a broken row is logged and never fatal; programs run under
  the cleared environment in a process group, found on `PATH` or by `ZETA_HANDLER_<PROGRAM>`; the
  handler report at startup; `OpError::Unknown { field, value, known }` for unknown ids.
- No channel-grid assumptions: the watch centres the decoder on the measured centre of the signal,
  never on a raster. The sonde rows' band is a range, not a list of centres.
- One new vendored third-party asset is allowed: Leaflet 1.9.x (BSD-2-Clause) under
  `crates/zeta-ui/web/vendor/leaflet/`, with its licence file, embedded like the other web files.
  Map tiles come from `https://tile.openstreetmap.org/{z}/{x}/{y}.png` with the OSM attribution,
  and only when the page is online; the e2e suite blocks that host and the tests pass without it.
- No new crate dependencies beyond what the workspace has, except a Reed-Solomon encoder if the
  builder finds the RS41 frame needs one and none is in-tree (author's call: `reed-solomon-erasure`
  is the wrong shape; a small in-tree GF(256) RS(255,231) encoder, about 80 lines, is preferred).
- The host-dependent tests pin the programs: `ZETA_HANDLER_RS41MOD` and friends point at
  `crates/zeta-ext/tests/fake_*` or a nonexistent path; a test that needs the real `rs41mod`
  skips with a named reason when it is absent, like the dump1090 tests do.
- The UI e2e suite stays under its 300 s deadline; map scenarios use fixture events over the
  websocket, not a radio.

## 4. Audio-input program rows

### 4.1 The row

`decoder.input.stream: audio` is accepted. An audio program row adds:

- `decoder.demod`: `{ "kind": "nbfm", "deviation_hz": N, "audio_rate_hz": N }` (author's call:
  `nbfm` only in this plan; `am` and `ssb` are the same shape later). Required when `input.stream`
  is `audio`, refused (`decoder.demod`, `audio input needs a demod`) when absent, and refused on an
  IQ row.
- `decoder.input.container`: `raw` (default) or `wav`. With `wav` the adaptor writes one RIFF/WAVE
  header before the samples (channels 1, `rate_hz`, 16-bit for `s16`, 32-bit float for `f32`,
  data and RIFF sizes `0xFFFFFFFF`, the streaming convention) so programs that insist on a header
  (`rs41mod`: "error: wav header") read stdin. Optional on an IQ row too (a 2-channel WAV for the
  `--iq2` family), author's call: allowed, not used by a built-in row.
- `input.format`: `s16` or `f32` for audio (already in the enum); `rate_hz` is the audio rate the
  program wants (48 000 for the rs1729 decoders).

Expansion (`template::expand`) of an audio program row is the chain
`channel($center_hz, $bandwidth_hz, $out_rate_hz) → nbfm_demod(deviation_hz, audio_rate_hz,
squelch: false) → external_decode { mode }`, pinned byte-for-byte by a test the way the four
in-crate rows are. The adaptor's converter takes audio frames instead of IQ blocks, resamples to
`input.rate_hz` when the demod's rate differs (it should not for a built-in row: the row sets both),
and writes `s16` or `f32`.

### 4.2 The sonde rows

Three rows, family `weather_science`, in `modes/`:

| id | name | program | notes |
|----|------|---------|-------|
| `rs41_decode` | Vaisala RS41 radiosonde | `rs41mod` | BoM's sonde; GFSK 4800 baud, one frame per second |
| `dfm_decode` | Graw DFM radiosonde | `dfm09mod` | DFM-09/17; ride-along |
| `m20_decode` | Meteomodem M20 radiosonde | `m20mod` | ride-along |

Shared shape (values author's calls; the builder verifies the program flags against the synthesised
frame and the real decoder):

- `band`: `{ "bandwidth_hz": 12500, "centers_hz": [], "range_hz": [400050000, 406000000], "min_hz": null }`.
  `range_hz` is a new optional `Band` field: scoring gives the 0.4 centre term when the measured
  centre lies inside it (section 6.3), and `modes list`/AGENTS.md print it. Rows with neither
  centres nor a range keep "anywhere".
- `modulation`: `["fsk", "fm"]`.
- `decoder`: `{ "kind": "program", "programs": ["rs41mod"], "args": ["--json", "--ecc", "--crc"],
  "demod": { "kind": "nbfm", "deviation_hz": 5000, "audio_rate_hz": 48000 },
  "input": { "stream": "audio", "format": "s16", "rate_hz": 48000, "via": "stdin", "container": "wav" },
  "output": { "via": "stdout", "port": null, "parser": "json_lines" },
  "install": { "debian": "sonde-decoders", "url": "https://github.com/rs1729/RS" } }`.
  `sonde-decoders` is the package name this host shows; it is not in Debian, so the report line's
  URL is the real hint. The handler report lists `rs41mod`, `dfm09mod` and `m20mod` separately.
- `event`: the rs1729 JSON line as it comes (`frame`, `id`, `datetime`, `lat`, `lon`, `alt`,
  `vel_h`, `heading`, `vel_v`, `sats`, `bt`, `batt`, and the `subtype`/`aux` fields when present),
  with the envelope on top. `t_ns` is the frame's own `datetime` (GPS time) when it parses, else
  the receive time, author's call: a sonde frame's time is its GPS time.
- `position`: `{ "lat": "lat", "lon": "lon", "alt_m": "alt" }` (section 7.1).
- `summary`: `"{id} {lat:.4},{lon:.4} {alt:.0} m {vel_v:+.1} m/s"`. The template language gains
  `{field:fmt}` with `fmt` one of `.N` (fixed decimals) and `+.N` (signed); a missing `?` field is
  omitted with its separator as today.
- `label`: `RS41`, `DFM`, `M20`; `protocol`: `rs41`, `dfm`, `m20`.
- `identify`: `{ "proof": "event", "min_events": 1, "dwell_ms": 5000, "min_span_ms": 1000, "quick": null }`.
- `catalogue`: the iNTERCEPT ids for radiosondes (the builder finds them in
  `crates/zeta-core/data/signatures.json` and the catalogue; `signatures.json` gains an RS41 entry
  with `decoder: "rs41_decode"` so `knowledge_lookup` names it).
- `test`: section 5.3.
- `notes`: where to find them (400-406 MHz, Brisbane Airport launches, the sonde's frequency varies
  by flight: use the measured centre), that one job decodes one sonde, and the `summary` form.

## 5. The synthesiser

### 5.1 Generic families

`SynthSpec` gains three families any row's `test` can name without new Rust:

- `fsk`: `{ "symbol_rate_baud": N, "deviation_hz": N, "order": 2 | 4, "bits": "<hex>" | "pattern:<hex>",
  "repeat": N }`: GFSK (Gaussian BT 0.5, author's call) at the symbol rate, the bits packed MSB
  first, repeated `repeat` times with a 50 ms gap. `order: 4` maps bit pairs to the four tones.
- `psk`: `{ "symbol_rate_baud": N, "order": 2 | 4, "bits": ... }`: BPSK/QPSK with a raised-cosine
  pulse (roll-off 0.35).
- `ook`: `{ "symbol_rate_baud": N, "bits": ... }`: on-off keying of a carrier.

Each takes the common `Impairments` (`snr_db`, `offset_hz`, `ppm`) and the new `echo`
(section 5.2). They exist for the detector's tests, the generic layer-4 work of plan 19 and the
later rows; no built-in row of this plan uses them for its own proof.

### 5.2 The echo impairment

`Impairments` gains `echo: { "delay_us": N, "gain_db": N } | null`: the signal plus a copy delayed
by `delay_us` and `gain_db` lower (multipath). `modes check` rows may use it; the APRS and POCSAG
rows gain a second `test` variant with a 10 us, -6 dB echo only if they still pass (builder's call
to drop a failing variant and say so in the plan).

### 5.3 The `rs41` family

`SynthSpec` family `rs41`: `{ "serial": "S1234567", "lat": -27.4, "lon": 153.1, "alt_m": 12000,
"vel_v": 5.0, "frames": 5 }` produces an IQ capture of `frames` RS41 frames one second apart at the
real line rate: the frame layout, the XOR scrambling mask, the per-block CRC-16 and the
Reed-Solomon (255,231) parity as `rs41mod` expects them. Author's call after the builder check:
the generator is written from the public protocol descriptions (the bazjo/RS41_Decoding wiki and
Vaisala's published frame notes), not ported from `rs41mod.c`: the rs1729 sources are not
Apache-compatible and only serve as the oracle. The frame layout, mask and code parameters are
interface facts; the code is ours. Subframe calibration data may be left zero where the decoder
tolerates it, and the GPS block carries the given position in ECEF with the given climb. It is
validated the only way that counts: `modes check rs41_decode` runs the synthesised capture through the row's own
chain and the real `rs41mod`, and `expect` is `{ "id": "S1234567" }` with `within_ms: 8000`. When
`rs41mod` is missing the check reports `skipped` as for dump1090. The synthesiser's own unit test
asserts the frame's CRCs and RS parity with the in-tree encoder, not the decoder.

Fallback (author's call): the generator has a budget of one task. If at the end of it `rs41mod`
still does not decode the synthesised frame, the RS41 row ships with `test: null` like the other
two, `modes check` reports it `skipped: no test`, the live acceptance (section 12) is its proof, and
the generator moves to the later wave with what was learned written in the plan; no recording goes
into the repo either way.

DFM and M20 have no synthesised frame in this plan: their rows have `test: null` and `modes check`
reports them `skipped: no test` (`--strict` fails them, which is right until a frame generator
exists). Both are listed under "later" in the plan.

## 6. The `watch` job

### 6.1 Shape

`job_start` with `kind: watch`, `center_hz` and `bandwidth_hz` the band to watch (400-406 MHz is
`center_hz: 403025000`, `bandwidth_hz: 5950000`; the operator chooses a centre off any channel of
interest since the HackRF's DC spike sits there). Like `scan` and `identify` it holds a rigid lease
on that band and runs a controller loop in the node (`node/watch.rs`, modelled on `identify.rs`).
Its graph is `fft → detect`, the `detect` stage new and in-crate (section 6.2); `graph put` on the
detect node tunes the thresholds. `job_context` carries `watch: { signals: [...], decoding: [...] }`.

Params (detect node, with defaults; author's calls, the builder tunes them on synthesised captures):

- `threshold_db` (10): a bin counts when it stands this far over the row's floor (`floor_db` of the
  spectrum row, plan 8's 2nd-percentile floor).
- `persist_ms` (3000): a run of bins must persist this long before it is a signal.
- `width_hz`: `[min, max]` (`[5000, 30000]`): runs narrower or wider are ignored (the sonde is about
  10 kHz; a 25 kHz FM voice channel still fits, a 200 kHz broadcast does not).
- `dc_mask_hz` (25000): bins within this of the capture centre are ignored; `edge_mask` (0.05): the
  outer 5 % of the capture on each side is ignored.
- `spurs_hz`: `[]`: centres the operator knows to be spurs (the HackRF's whole-MHz lines), each
  masked ±`dc_mask_hz / 5`.
- `gone_ms` (30000): a signal unseen this long is `gone`.

### 6.2 Events and the loop

The `detect` stage reads spectrum rows and publishes `signal` events: `{ "state": "new" | "gone",
"center_hz", "bandwidth_hz", "snr_db", "first_ns", "last_ns" }`, `center_hz` the power-weighted
centre of the run and `bandwidth_hz` its width at `threshold_db`. The loop:

1. On `new`: if the signal overlaps a band some running job already holds, or one the watch is
   already handling, ignore it. Else start an `identify` child on `center_hz` with the row-width
   `bandwidth_hz` the best-scoring fitting row expects (12 500 Hz for the sonde rows; the sweep's
   scoring, section 6.3, decides the order), inside the watch's capture. Publish `watch` event
   `{ "state": "identifying", "signal": ..., "job": <identify id> }`.
2. On the child's verdict: a proof leaves that decoder running (identify's rule) under
   `verdict.job`. The watch then applies its placement policy (section 6.4): keep that job where it
   is, or start the same row on a free source and stop the child's job. Publish
   `{ "state": "decoding", "mode", "job", "source", "signal" }`; `watch.decoding` lists it.
   An exhausted identify publishes `{ "state": "unknown", "signal", "features" }`; the signal is
   retried after `retry_ms` (60 000) while it persists, at most three times, then left alone until
   it goes and comes back.
3. A decoder the watch started is stopped when its signal is `gone` and no event arrived for
   `silence_ms` (120 000) (a landed sonde), publishing `{ "state": "lost", "job", "signal" }`.
4. Stopping the watch stops its identify child and every decoder it started (a watch owns what it
   starts; a decoder the operator wants kept is restarted by hand, or the watch is left running).
   On a node restart the decoders resume as ordinary jobs and the watch re-adopts them by id from
   its persisted context, stopping any whose signal does not reappear within `gone_ms`.

One identify at a time per watch (the radio's capture is shared; identify's children are serial
anyway). Signals that arrive while one is in progress queue in score order.

Job records (author's call after the builder check): the identify runs a watch starts, and their
per-row children, are ephemeral: their records carry `owner: <watch job id>` and are dropped from
the history the moment they stop instead of taking one of the 20 kept slots, and the page hides
owned jobs from the jobs pane while they run (the ops log still records every start and stop). A
watch over a band full of carriers and spurs must not push the operator's own history out. The
decoders the watch keeps (`decoding` entries) are ordinary jobs with ordinary records, so a chase
survives in `job list` and `transcripts`-style tooling.

`graph_put` on the watch (threshold tuning) restarts the watch loop under the same id and
re-adopts its running decoders from the persisted context, exactly as a node restart does; only
`job_stop` on the watch stops what it started. The re-adoption path is therefore the general
mechanism and gets its own tests.

### 6.3 Scoring with a range

`score_rows` gives the 0.4 centre term when the measured centre lies within `band.bandwidth_hz` of a
listed centre, or inside `band.range_hz`, or when the row lists neither. In 400-406 MHz the sonde
rows therefore outscore pagers and APRS (listed centres elsewhere) and tie with each other (equal
family, then id: `dfm_decode`, `m20_decode`, `rs41_decode`). Author's call: a row may carry
`identify.priority` (integer, default 0, higher first) as the first tie-break, and the RS41 row sets
1 so the common sonde is tried first. `classify` is unchanged except for the range term.

### 6.4 Placement: the free tuner

Policy params on the watch (graph params of the detect node, author's call to keep one node):

- `decode_on`: `"capture"` (default) or `"free"`. With `capture` the proved decoder stays where
  identify left it, inside the watch's capture: one radio does both. With `free` the watch asks the
  lease manager for a free source (section 8) that covers the signal's band, prefers the ids in
  `prefer` (`[]`) in order, starts the row there on the measured centre and stops the child's
  job; when none is free it falls back to `capture` and says so in the `decoding` event
  (`"placed": "capture" | "free"`).

A source on another node (`--source-node`) is a candidate like any other; the watch does not know
or care that it is remote. A remote link ships 250 kHz unless widened, which is enough for a 12.5
kHz sonde channel.

### 6.5 The page and the agent

The page lists a `watch` job like a scan (its band outlined, `watch.signals` drawn as small
markers in the band, `decoding` entries as the decoder jobs they are). The decode menu gains
"Watch this band" on a selection. The agent guide explains the watch in field craft ("Watching a
band for sondes") with the one-radio and the two-radio form.

## 7. Positions and the map

### 7.1 The envelope

A row may carry `position: { "lat": "<field>", "lon": "<field>", "alt_m": "<field>" | null }`.
`summary::fill` adds `position: { "lat", "lon", "alt_m" }` (numbers, `alt_m` null when the field is
absent) to every event whose named fields are present numbers; an event without them gets no
`position`. APRS packets with a position report get one in the later wave by setting the map on
the row; nothing is inferred from field names.

### 7.2 The map layer

A new page module `map.js` with vendored Leaflet:

- `view set map on|off` (default off; persisted in the view like the other policies) shows a map
  pane: wide renderer beside the waterfall (the jobs pane's width, author's call), narrow renderer
  as a third tab beside waterfall and transcript. `view set home LAT,LON` sets the home marker
  (persisted; the builder may seed it from the browser's geolocation when the operator allows).
- Every running job whose events carry `position` gets a marker and a track (polyline of its last
  `TRACK_POINTS` 3600 fixes), the marker's popup the last `summary`, its age, and distance and
  bearing from home when home is set. The jobs pane row of such a job shows the same distance and
  bearing text.
- Tiles from OSM when a fetch succeeds; a failed tile leaves its square blank; the attribution is
  always drawn. The e2e suite aborts the tile host.
- Markers are coloured per job like ribbons; a job's marker disappears when it stops.

The map reads the same frames the ribbons do, so the summary ribbon of a sonde and its map marker
agree to the frame.

## 8. Free-tuner allocation

The lease manager gains `free_sources(center_hz, bandwidth_hz) -> Vec<SourceId>`: sources with no
rigid holder, no pin and no fault whose `tune_min_hz..tune_max_hz` covers the band and whose
`max_bandwidth_hz` is at least `bandwidth_hz`, in `source list` order. Elastic jobs do not make a
source busy (they never block a retune anywhere else; the page's spectrum follows the new
capture), and the watch's own source is never a candidate for its decoders under `free`. A
`--source-node` source can look free here while the other node has its own jobs on the radio;
that surfaces as a `conflict` on the `job_start`, so the watch treats a conflict on a chosen source
as "not free after all", tries the next candidate, and falls back to `capture` when none is left,
saying which in the `decoding` event. The watch uses it.
It is exposed as the read-only op `source_free` (`{ "op": "source_free", "center_hz", "bandwidth_hz" }`,
CLI `zeta source free --center-hz --bandwidth-hz`) answering `{ "free": [ids], "busy": [{ "id",
"holders", "elastic", "pin", "fault" }] }`, so the agent can answer "which radio can take this"
without reading `source list` itself. The AGENTS.md note says a free source is a suggestion, not a
reservation: the next `job_start` may still conflict.

## 9. The feed sink (last)

A new stage kind `sink_sondehub` (`inputs: event`, no outputs) in `zeta-ext`: it takes events with
`protocol` in `rs41 | dfm | m20` and `position`, batches them for `batch_ms` (5000) and POSTs
SondeHub's telemetry format to `https://api.v2.sondehub.org/sondes/telemetry` with the station's
callsign and position. Params: `callsign`, `lat`, `lon`, `alt_m`, `batch_ms`, `url` (overridable for
tests). It is added by `graph get`, edit, `graph put` on a running sonde job (the job restarts), or
by a `sondehub` field on the watch's params that appends it to every decoder the watch starts; the
operator gives the callsign and position, nothing uploads otherwise. Errors are counted in
`job_context.sondehub` (`sent`, `failed`, `last_error`), never retried in a loop beyond the next
batch. The `StreamKind::Sink` payload (today an `IqBlock`) is not involved: a sink stage consumes
events and publishes nothing. This section is the last task of the plan and is deferred to the
later wave if the plan runs long; the design is fixed here so a deferral costs nothing.

## 10. Later waves

Rows only, on these frameworks: M17 and DMR voice (Codec 2 in-crate, AMBE through
`tools/install-vocoders.sh`) feeding `transcribe`; AIS; FLEX; whole-span POCSAG and APRS as a watch
with `decode_on: capture` and many signals; `rds_decode` pilot report and `wfm_demod` stereo; DFM
and M20 frame synthesis; APRS positions on the map. Trunking stays out.

## 11. Left to the builder

- The exact `rs41mod` flags (`--ecc`, `--crc`, `--ptu`, `--sat`), checked against the synthesised
  frame; whether `--IQ`/`--iq2` IQ input is worth a second row variant (not in this plan).
- The RS41 frame generator's internals (layout from `rs41mod.c`, which subframes may be zero).
- The detect stage's run-grouping and the thresholds' defaults, tuned on synthesised captures with
  a sonde-shaped `fsk` burst in noise plus a DC spike and spurs.
- Where the watch loop and identify share code (`launch_identify` already repeats `launch_scan`'s
  scaffolding; a shared controller-job helper is welcome).
- The map pane's layout in both renderers and the Leaflet version (1.9.4 at the time of writing).
- How `t_ns` is taken from the rs1729 `datetime` (ISO 8601, GPS time) and what happens when it is
  absent.
- Whether the three rows share one `programs` family entry in the handler report or three lines.

## 12. Live acceptance (plan 17)

On the home station with the discones back, during a Brisbane launch window: a `watch` on
400-406 MHz on the HackRF reports the sonde as a `signal` within `persist_ms` of its first frame,
identifies it as `rs41_decode` within one dwell, and `job_context.watch.decoding` names a running
`rs41_decode` job whose events carry `id`, `position` and a `summary`; the page's map shows the
marker moving and the ribbon scrolling; `view set home` gives distance and bearing; with the field host's
B210 attached as a `--source-node` and `decode_on: free`, the decoder lands on the B210 and the
HackRF keeps watching; stopping the watch stops the decoder; `zeta modes check --strict rs41_decode`
exits 0 on this host (a whole-tree `--strict` run fails on `dfm_decode` and `m20_decode` until they
have a test, which is correct and not a pass criterion); `zeta source free` lists the idle radio. Nothing uploads until a `sink_sondehub` node with a callsign is put on the job.
