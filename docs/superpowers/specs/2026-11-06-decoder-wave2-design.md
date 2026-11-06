# Plan 29: decoder wave 2, rows and small stages

Design spec, 2026-11-06. Written from the operator's gists in `TODO.md` ("Feature: more digital
decoding modes", "Feature: decoder wave 2 (rows on the sonde frameworks)", "UI: state-stream
decoders beyond the first subjects", the Feeds/Time/Vocoders bullets of "Architecture: a mode
registry and a generic external-decoder adaptor") on the frameworks plans 16, 17 and 23 delivered
(`2026-10-27-mode-registry-design.md`, `2026-10-28-sonde-frameworks-design.md`,
`2026-11-02-subjects-design.md`). A reviewer read it as the builder before hand-over; its
findings are folded in (where a graph row's stage may live and the handler lookup, the check's
skip rule for graph rows, the CI gate's order, the e2e fixture's home, four citation fixes) and the
two calls it asked for are settled under section 15; nothing new went to the operator.

Decisions here are the author's unless marked "builder's call" (left open) or listed in section 14
(the operator's).

## 1. Intent and why

Plan 17 said it plainly: the sonde was the proving mode, and "the later wave (voice, AIS, FLEX,
whole-span pagers and APRS, stereo) is rows on top of these frameworks, not new machinery". This is
that wave, cut to what is rows and small stages:

1. **AIS** (161.975 / 162.025 MHz, GMSK 9600): vessels on the map with their names, the one
   maritime signal every VHF antenna near a port hears.
2. **FLEX** pagers (1600 / 3200 / 6400 bps): the pager protocol the node cannot read yet, beside
   POCSAG.
3. **M17** digital voice (4FSK 4800 baud, Codec 2, an open protocol): the first digital voice
   row, whose audio feeds `job attach --kind transcribe` unchanged.
4. **DFM and M20 frame synthesis**, so `zeta modes check --strict` stops failing the two sonde
   rows that ship with `test: null`.
5. **APRS positions on the map**: the APRS row has a subject (`src`) and the map has per-(job,
   key) tracks since plan 23, but the decoder never parses the position out of `info`, so no
   station appears beside the sondes.

Out of scope, by the operator's cut: DMR and P25 voice and the AMBE/IMBE install script,
whole-span watches (POCSAG and APRS over a pin), VDL2 and HFDL, the map tab, feeds out (APRS-IS,
AIS NMEA sinks), trunking.

What this document adds beyond rows is small and generic, and each piece is named as such in
section 4: a tolerance form in `modes check`'s `expect`, program handlers on graph rows (for the
vocoder), the stream kinds a row's job emits in `modes_list` (so the page knows an audio row), the
rows' `notes` rendered into AGENTS.md, and a fifth quick attempt (`ais`).

## 2. Goals, non-goals, anti-goals

### Goals

- `ais_decode`, `flex_decode` and `m17_decode` are embedded rows, each `ready` on a host that has
  what it needs, each with a synthesised test `zeta modes check` passes, each listed in
  `knowledge_lookup` as the `decoder` of its catalogue entries.
- AIS vessels are subjects: one table row per MMSI with name, callsign, speed and course, one
  marker and track per vessel on the map, built from the position and static messages the decoder
  sees.
- M17 audio is ordinary job audio: the page's play and Transcribe buttons work on an `m17_decode`
  job, `job_attach --kind transcribe` works, and the stream's callsigns are the job's subjects.
- `dfm_decode` and `m20_decode` have a `test` the real `dfm09mod` and `m20mod` decode, written
  from public descriptions, never from the rs1729 sources; `zeta modes check --strict` passes
  every embedded row on the field host.
- An APRS station with a position in any of the three common formats appears on the map and in
  the Subjects table with its fix, on the existing per-station track.
- Every row's prose reaches the agent: the row's `notes`, one sentence in the `job_start` note and
  a bullet in field craft, generated and checked as today.

### Non-goals

- No in-crate FLEX decoder in this wave (multimon-ng is the decoder; the in-crate one is a slot,
  section 6.4). No in-crate Codec 2 (section 7.2 says why).
- No AIS or APRS feed out (the Feeds gist). The AIS event carries its `nmea` sentence so a later
  sink has it; nothing leaves the node.
- No M17 packet-mode content, no decryption of anything (section 11).
- No whole-span POCSAG/APRS watch, no stereo, no object/item positions for APRS (listed as
  leftovers in section 13).
- No new page view. The Subjects tab and the map take the rows as they are; the only page change
  is that an audio-bearing row's job gets the play and Transcribe buttons (section 4.3).
- No change to the transcriber, the mix, the watch or `identify`'s loop.

### Anti-goals

- A row listed `ready` without a passing check on a host that has its program. Every row here has
  a synthesised test; a program row's real decode is proven by `--strict` on the field host and
  recorded in `checked`, as plan 16 ruled.
- A copyleft dependency in the workspace. `deny.toml` forbids GPL, LGPL and AGPL and CI runs
  `cargo deny`; this document does not ask for an exception (the question is the operator's,
  section 14).
- A generator derived from a GPL decoder's source. The DFM, M20, FLEX and M17 synthesisers follow
  the RS41 rule (plan 17 section 5.3): public descriptions, the external program as the oracle only.
- A pager row that treats message content differently from POCSAG without the operator's word.
  FLEX follows the POCSAG row exactly (section 11).
- An inner graph edge carrying events at frame rate. Plan 27 feeds every inner event edge to the
  source's feed and the page counts each frame; a decoder chain that publishes 25 events a second
  per voice frame would swamp both. Each row here is one stage from IQ to its outputs.

## 3. Constraints

- **Rows.** `crates/zeta-core/src/modes/row.rs`: every field required unless defaulted, unknown
  fields rejected, `_hz`/`_ms`/`_ns`/`_baud` fields integers, every field the summary, `position`,
  `time` and `subject` name must be in `event.schema.properties`. A graph row's template
  type-checks against the stage registry at load; a program row needs `programs`, `args`, `input`,
  `output`, `install`, and `demod` for audio input (`Demod` has one variant, `nbfm`). `quick` must
  be one of `QUICK_ATTEMPTS` (`aprs`, `pocsag`, `acars`, `rds`). `subject.key` may be a number
  (`subjects.js` `keyed` takes string, number or boolean and keys by its text).
- **Expansion.** `template.rs::expand` fills a graph row's `channel` from `$center_hz`,
  `$bandwidth_hz`, `$out_rate_hz`; the caller passes `out_rate_hz` from `ops.rs`'s
  `channel_rate_hz(source, bandwidth)`, at least `CHANNEL_RATE_MIN_HZ` (48 000). A 48 kHz channel
  gives 5 samples per symbol at 9600 baud, over `MIN_SAMPLES_PER_SYMBOL` (4, in `zeta-core`'s
  `bits.rs` and `zeta-dsp`'s `lib.rs`). A program row becomes `channel → [nbfm_demod] →
  external_decode`, the adaptor resampling to `input.rate_hz`.
- **Where a graph row's stages may live.** An embedded graph row is type-checked against a
  `StageRegistry` at load, and `crates/zeta-core/tests/modes.rs` builds that registry from
  `zeta_dsp::register_all` alone (`zeta-core` cannot depend on `zeta-ext`). A stage that only
  `zeta-ext` registers (today `external_decode`, `sink_sondehub`) is reached through program rows
  or `graph put`, never named by an embedded graph row: such a row would be `broken` in
  `embedded_rows_parse_and_validate`. So every stage an embedded graph row names lives in
  `zeta-dsp`. `zeta-dsp`'s tokio has no `process` feature today; `zeta-ext`'s `proc.rs` holds
  `find_handler` (the `ZETA_HANDLER_*` override, then `PATH`).
- **The check.** `crates/zeta/src/modes_check.rs`: the first event whose fields match `expect`
  passes; `compare` is exact equality per named field. A row without a test is `skipped: no test`,
  a missing program `skipped: <program> missing`, both failures under `--strict`. CI runs the
  non-strict check (`crates/zeta/tests/modes_check.rs`, `modes_check_passes_the_four_in_crate_rows`).
- **The synthesiser.** `crates/zeta-dsp/src/synth.rs`: `SynthSpec` is a tagged enum (`family`),
  each family validates its parameters and names its capture rate; `gfsk_modulate(symbols, order,
  baud, deviation_hz, rate_hz)` is Gaussian BT 0.5 for order 2 or 4; `hdlc_frame_bits`,
  `nrzi_encode`, `ax25_ui_frame`, `pocsag_encode` (a BCH(31,21) encoder inside) and `awgn` exist;
  `Impairments` are `snr_db`, `offset_hz`, `ppm`, `echo`. `rs41_frame.rs` is the template for a
  frame generator: a doc comment that cites its public sources and states which conventions were
  checked against published example frames, the oracle named, the polarity recorded.
- **Stages.** `Stage` (`zeta-core/src/stage.rs`) declares inputs and outputs by `StreamKind`;
  `node_outputs` in `launch.rs` gives a job one output stream per kind its terminal stage emits.
  Library pieces to reuse: `nbfm::nbfm_demod` (the discriminator the POCSAG stage runs from IQ),
  `ax25::HdlcDeframer` (flags, unstuffing, CRC-16/X.25, LSB-first bytes: AIS framing is the same
  HDLC), `ax25::parse_ax25_ui`, `crc.rs` (`CRC16_CCITT`, `CRC16_X25` and friends), `pocsag::bch_ok`,
  `util::StreamingFir`, `resample::Resampler` (any rate pair).
- **Programs.** `crates/zeta-ext`: `external_decode` resolves its program from the registry at
  launch (never from graph params), runs it with the cleared environment, and `probe_handlers`
  (`crates/zeta/src/node_setup.rs`) prints one report line per distinct `programs` list; the
  parsers live in `crates/zeta-ext/src/parsers/`. `nmea.rs` decodes `!AIVDM` far enough for
  `msg_type` and `mmsi` and says "Full AIS fields arrive in plan 17"; `multimon.rs` reads POCSAG
  lines and passes FLEX lines through as `{text}`, with a FLEX_NEXT sample in its test:
  `FLEX|2026-10-28 12:00:00|1600/2/K/A|05.120|001234567|ALN|HELLO`. `zeta-ext` depends on
  `zeta-dsp` (the `kiss` parser uses `ax25`), not the other way.
- **Licences.** `deny.toml`: copyleft is not allowed, the workspace is Apache-2.0. On crates.io:
  `codec2` 0.3.1 is "LGPL-2.1-only AND MIT" (a pure-Rust port), `codec2-sys` binds libcodec2
  (LGPL-2.1), `ais` 0.12 is Apache-2.0 (a sentence parser, not a demodulator), `m17core` 0.1.0 is
  MIT. On the field host today: `/usr/bin/c2dec` and `c2enc` (package `codec2` 1.2.0),
  `multimon-ng` 1.5.0, `rs41mod`, `dfm09mod`, `m20mod`.
- **The page.** `agent.js` `AUDIO_KINDS` is a fixed list (`fm_demod`, `wfm_demod`, `am_demod`,
  `ssb_demod`, `mix`): only those rows get play and Transcribe. `subjects.js` fetches the rows and
  keys events by `subject.key`; `map.js` tracks a keyed row per `${job}\u0000${key}` with
  `track_points`/`track_min_m` (plan 23 section 4.5 named APRS 100 / 50 m). The e2e suite runs
  about 474 s of its 600 s deadline (plan 27 note), so this wave adds at most one scenario.
- **Docs.** AGENTS.md and `skills/zetasdr/SKILL.md` are generated (`cargo run -q -p zeta --
  agents-md`) from `ops.rs` doc text, `agents_md.rs` `note()` strings and
  `docs/agents/field-craft.md`; CI fails on drift. `row.rs` documents `notes` as "free text the
  generated AGENTS.md carries for the mode", but `agents_md.rs` never reads `notes` (nothing in
  `crates/zeta/src` does): the three sonde rows' notes reach `modes_show` only.
- **Public repository.** Roles, not host names; the demo station is Brisbane Airport (-27.3842,
  153.1175, 4 m), which the tests' positions use.

## 4. Shared pieces (generic, built first)

### 4.1 `expect` tolerances in `modes check`

`compare` accepts, for any field, `{"near": N, "within": D}`: the event's field must be a number
with `|got - N| <= D`; every other value stays exact. The diff line reads `lat: expected within
0.0001 of -27.3842, got -27.39`. About 25 lines plus a test in `modes_check.rs`. It exists because
positions never round-trip exactly: AIS encodes 1/10 000 of a minute, APRS uncompressed 1/100 of a
minute, the sonde decoders print what their floating point gives. `rs41_decode`'s test may then add
its `lat`/`lon` (builder's call; today it checks `id` only).

### 4.2 Handlers on graph rows

`Decoder::Graph` gains `handlers: [{"programs": [...], "install": {...}}]`, optional, default
empty: programs a stage of the graph needs on the host. `probe_handlers` (which iterates program
rows only today) probes them exactly as it probes program rows' `programs` (grouped by list, one
report line, `ZETA_HANDLER_<PROGRAM>` honoured), and a graph row with a missing handler is
`unavailable` with that line as `reason`. `modes check` skips an `unavailable` graph row as
`skipped: <program> missing` exactly as it skips a program row (`check_row` tests
`Decoder::Program` today; it tests the state instead), so CI's pinned skip lines hold on a host
without `c2dec`.

The stage that runs the handler lives in `zeta-dsp` (the constraint above) and finds the program
itself at launch: `find_handler` moves from `zeta-ext/src/proc.rs` into `zeta-core`
(`modes/handlers.rs`, standard library only, so no new `zeta-core` dependency), `probe_handlers`
and `external_decode` call it there, and the stage calls it on the row's `handlers[i].programs`
read from `StageContext.mode`, naming the handler by its index in the row. Probe and stage share
one lookup, so they cannot disagree; the probe's result is what `modes_list` shows, the stage's
lookup is what runs. Graph params never carry a path, so the plan 16 rule stands: a program arrives
only through a row file on the node's host. `modes_list`/`modes_show` show `program` for the first
handler as today. `zeta-dsp`'s tokio gains the `process` feature. About 150-200 lines in `row.rs`,
`mod.rs`, `handlers.rs`, `node_setup.rs`, `modes_check.rs`.

### 4.3 What a row's job emits

`modes_list` items gain `outputs`: the stream kinds the row's job publishes, derived at load from
the template's terminal stage specs (`["event"]` for every row today, `["event", "audio"]` for
`m17_decode`). The page replaces the `AUDIO_KINDS.includes(job.kind)` test by "a built-in audio
kind, or a ready row whose `outputs` has `audio`" (the rows are already fetched for the subjects
store), so the play and Transcribe buttons appear on an `m17_decode` row. The `job_attach` doc text
says "a job whose kind emits audio: the voice demodulators and any mode row whose `outputs` lists
`audio`". About 60-100 lines across `ops.rs`, `mod.rs`, `agent.js`.

### 4.4 Row notes in AGENTS.md

The "Decoding modes" part of the generated guide gains one line per embedded row that has
`notes`: "`<id>`: <notes>". With that, a new row's prose has one home for the row-specific part (the
row file) and the generated text cannot drift from it. `agents_md.rs` reads
`ModeRegistry::embedded()`. About 30 lines; the existing three sonde notes appear at once.

### 4.5 A fifth quick attempt

`QUICK_ATTEMPTS` gains `ais` and `identify.rs` `quick_attempt` gains its arm (section 5.3). The
generated texts that enumerate the four names (`note("classify")`, field craft) say five.

### 4.6 The multimon parser's FLEX lines

`parsers/multimon.rs` reads FLEX lines (section 6.2); every other non-POCSAG, non-FLEX line stays
`{text}`.

## 5. AIS

### 5.1 In-crate, and why

In-crate. The pieces are already in `zeta-dsp`: AIS framing is HDLC with the X.25 CRC-16 and
NRZI, which `ax25::HdlcDeframer` and the APRS stage do today; GMSK at 9600 baud is a discriminator
and a bit clock, which the POCSAG stage does from IQ at other rates; the payload parser is bit
fields, about 350 lines for the message types this wave reads. The external alternatives do not
fit the adaptor: `rtl-ais` (packaged) reads an RTL-SDR directly, not IQ on stdin; AIS-catcher is
not in Debian or Ubuntu and ships its own `.deb`; `gnuais` is dead. The `nmea` parser stays for an
operator row that wraps a program printing `!AIVDM`, and section 5.2 makes it emit the same fields
as the in-crate stage, so both kinds of row produce one AIS event shape.

### 5.2 The decoder and its events

One stage, `ais_decode`, IQ in, `event` out (the anti-goal in section 2 rules out a
`fsk_demod → clock_recover → slicer → ais_frame` chain: its inner `bits` edges are events the feed
would carry). Inside: `nbfm_demod` discriminator (deviation 2400 Hz: GMSK with modulation index
0.5 at 9600 baud), a low-pass at 0.6 of the symbol rate, a bit clock at 9600 baud pulled by the
transitions as `Afsk1200Demod` does, NRZI decode, `HdlcDeframer`, then each frame's bytes
bit-reversed (HDLC sends bytes least significant bit first, the AIS payload is read most
significant bit first; the convention is pinned by the self-check in section 12 and by the
published sentence in `nmea.rs`'s test) and parsed by a new library module `ais_msg.rs`:

- message types 1, 2, 3 (class A position), 4 (base station), 5 (class A static and voyage), 18
  (class B position), 21 (aid to navigation), 24 (class B static, parts A and B: class B boats
  have no type 5, so without 24 they never get names). Every other type is framed, counted and
  reported as an event with `mmsi`, `msg_type`, `nmea` and the rest null: the payload is public,
  but reading it is a later row's work.
- `ais_msg::encode_sentences(payload_bits) -> Vec<String>`: the `!AIVDM,<n>,<i>,<seq>,<ch>,<payload>,<fill>*<cs>` sentence(s) for the frame, split at 60 payload characters as receivers do (a type 5 spans two).

`parsers/nmea.rs` calls `ais_msg::decode` on its unarmoured bits, so its events gain the same fields
(its `channel`, `payload` and `fill_bits` stay).

Event fields (the row's schema): `protocol`, `mmsi` (integer; the key), `msg_type` (integer),
`channel` (`"A"`, `"B"` from the job's centre, else null), `repeat` (integer), `nmea` (array of
strings), `lat`, `lon` (number or null; AIS's 91 / 181 "not available" become null), `sog_kt`
(number or null; 102.3 is null), `cog_deg` (number or null; 360 is null), `heading_deg` (integer or
null; 511 is null), `nav_status` (integer or null), `name` (string or null, trailing `@` and spaces
trimmed), `callsign`, `destination` (string or null), `imo`, `ship_type`, `aton_type` (integer or
null), `virtual_aton` (boolean or null). `summary`:
`"{name?mmsi} {lat?:.4},{lon?:.4} {sog_kt?:.1}"` (the columns carry the units).

### 5.3 The row

```
id ais_decode, name "AIS vessel traffic", family maritime,
band {bandwidth_hz 25000, centers_hz [161975000, 162025000], min_hz 12500, range_hz null},
modulation ["fsk", "fm"], label "AIS", protocol "ais",
decoder graph: channel → ais_decode,
position {lat, lon, alt_m null},
identify {proof event, min_events 1, dwell_ms 10000, min_span_ms 1000, quick "ais", priority 0},
catalogue ["ais", "automatic-identification-system-ais"],
subject {key mmsi, name "vessels", waterfall summary,
         columns name/Name, callsign/Callsign, sog_kt/Speed kt, cog_deg/Course deg,
                 destination/Destination,
         heading cog_deg, label name, track_points 200, track_min_m 50},
notes "AIS on 161975000 Hz (channel A, 87B) and 162025000 Hz (channel B, 88B), 25000 Hz band,
       one job per channel. A moving class A ship reports every 2-10 s, one at anchor every
       3 min, class B every 30 s; names arrive in separate static messages, so a vessel's row
       fills over a few minutes."
```

`waterfall: summary` because one channel in a port hears dozens of vessels (plan 23's rule: many
subjects, one line). `quick: "ais"` runs the same chain over `classify`'s buffered span and proves
on the first CRC-clean frame; `dwell_ms` 10 000 covers an under-way class A report interval, not a
vessel at anchor (the notes say so). A channel plan `ais_vhf` with the two channels in one 100 kHz
capture is one row in `plans.rs` for `plan_start` (builder's call whether this wave adds it; it is
twenty lines and the acceptance in section 12 wants both channels).

### 5.4 The synthesiser

Family `ais`: `{ "mmsi": 503000001, "lat": -27.3842, "lon": 153.1175, "sog_kt": 12.3,
"cog_deg": 45.0, "name": "ZETA TEST" | null, "repeat": 3 }`. It builds a type 1 position report
(168 bits) and, with `name`, a type 5 static report after it; each frame is 24 bits of training
(`0101...`), the start flag, the payload bytes least significant bit first, the CRC-16/X.25, the end
flag (`hdlc_frame_bits`), NRZI (`nrzi_encode`), GMSK at 9600 baud and 2400 Hz deviation
(`gfsk_modulate`; AIS transmitters use BT 0.4, the receiver filter is 0.5: builder's call whether
`gfsk_modulate` gains a `bt` argument), 26.7 ms slots at 48 kHz, `repeat` frames `gap_ms` apart.
Validation: MMSI 9 digits, lat/lon in range, speed under 102.2, course under 360, name at most 20
characters of the AIS 6-bit set. The row's test: `expect {mmsi: 503000001, msg_type: 1, lat:
{near: -27.3842, within: 0.0001}, lon: {near: 153.1175, within: 0.0001}}`, `snr_db 20`,
`within_ms 5000`.

### 5.5 Size

`ais.rs` stage 250-350, `ais_msg.rs` 350-450 (shared with the `nmea` parser), the quick attempt
60-80, the family 200-250, tests 250-300: about 1100-1450 lines.

## 6. FLEX

### 6.1 multimon-ng, and why

A program row running multimon-ng (`multimon-ng` 1.5.0 is packaged on Debian and Ubuntu and
installed on the field host). FLEX in-crate is a real decoder (four sync patterns for the three
rates, the frame information word, 11 interleaved blocks of BCH(31,21) words, vector and message
word types, four phases at 6400 bps): 900-1400 lines by this document's estimate, against about
100 for the parser branch and the row. Plan 16's handler-report example (the mode-registry spec,
section 5.3) already names `flex_decode` as a multimon-ng row. The in-crate decoder is a slot
(section 6.4), and the synthesiser this wave writes serves both.

### 6.2 The row, the parser, the events

```
id flex_decode, name "FLEX pagers", family pagers,
band {bandwidth_hz 25000, centers_hz [], min_hz 12500, range_hz null},
modulation ["fm", "fsk"], label "FLEX", protocol "flex",
decoder program: programs ["multimon-ng"],
        args ["-t", "raw", "-a", "FLEX_NEXT", "-q", "-"],
        demod {kind nbfm, deviation_hz 4800, audio_rate_hz 22050},
        input {stream audio, format s16, rate_hz 22050, via stdin, container raw},
        output {via stdout, port null, parser multimon},
        install {debian "multimon-ng", url "https://github.com/EliasOenal/multimon-ng"},
identify {proof event, min_events 1, dwell_ms 15000, min_span_ms 1000, quick null, priority 0},
catalogue ["flex"]   (not "reflex": ReFLEX is the two-way variant on other channels),
subject null,
notes "FLEX pagers at 1600, 3200 or 6400 bps (2- or 4-level FSK, ±4800 Hz), 25000 Hz band;
       929-932 MHz in the US, 151.8-153.4 MHz elsewhere per the catalogue; start it on the
       measured centre. One event per page, like POCSAG. It runs multimon-ng on the node's host."
```

`centers_hz` is empty: this document knows no live FLEX centre in Australia (section 14). The
`multimon` parser gains the FLEX branch, both line forms multimon-ng prints:
`FLEX|<time>|<baud>/<levels>/<K|C>/<phase>|<cycle>.<frame>|<capcode>|<ALN|NUM|TON|BIN>|<text>`
(FLEX_NEXT, the form in the parser's existing test) and
`FLEX: <time> <baud>/<levels>/<K> <cycle>.<frame> [<capcode>] <ALN> <text>` (the older `FLEX`
demodulator). Fields: `capcode` (integer), `baud` (integer), `levels` (integer), `phase` (string or
null), `cycle`, `frame` (integer), `kind` (`alpha`, `numeric`, `tone`, `binary` or null), `text`
(string or null). multimon's timestamp is the host clock and is dropped (`t_ns` is the frame's).
`summary`: `"{capcode}: {text?}"`, POCSAG's shape. No subject, as POCSAG: each page is its own
message (the "address list in the Subjects tab" gist stays open). The exact lines are pinned
against output captured from multimon-ng on the field host during `--strict` (builder's task; the
format in the parser's test is the starting point).

### 6.3 The synthesiser

Family `flex`: `{ "capcode": 1234567, "text": "HELLO ZETA", "baud": 1600, "repeat": 2 }`, 2-level
only in this wave (`baud` 1600; 3200/6400 refused with "this wave synthesises 1600 bps"). It writes
one 1.875 s frame: bit sync, the A and B sync words for 1600/2, the frame information word, the
second sync, then the 11 blocks of 8 BCH(31,21)-coded words interleaved as the protocol sends them,
holding one short address, its vector word and the alphanumeric message words for `text`, idle words
elsewhere; 2-FSK at 1600 baud and ±4800 Hz over NBFM at 48 kHz. The BCH encoder is the one
`pocsag_encode` uses, lifted into `crc.rs` or a `bch.rs`. Sources: the public FLEX descriptions
(the sigidwiki page, the Motorola FLEX patents' frame descriptions, Motorola's published protocol
specification if the operator has it); never multimon-ng's `flex.c` or `flex_next.c` (GPL). The
oracle is multimon-ng itself: `zeta modes check --strict flex_decode` on the field host. Expect
`{capcode: 1234567, text: "HELLO ZETA"}`, `within_ms 6000`. Fallback as plan 17 section 5.3: one
task's budget; if multimon-ng does not decode the synthesised frame by then, the row ships with
`test: null`, `modes check` prints `skipped: no test`, and the plan records what was learned. About
400-600 lines.

### 6.4 The in-crate slot

When an in-crate `flex_decode` stage arrives (a later wave), the row turns into a graph row with
the same `event` schema and the same test, and plan 16's rule applies: the row's test proves the two
decoders equal.

### 6.5 Size

Parser branch 80-120, row 100, family 400-600, tests 150-200: about 700-1000 lines.

## 7. M17

### 7.1 Shape

A graph row, `channel → m17_decode`, one stage with two outputs, `event` and `audio`. The stage
lives in `zeta-dsp` (an embedded graph row may name no other crate's stage, section 3; it runs a
program through section 4.2's lookup) over a library module `zeta-dsp/src/m17.rs`:
4FSK symbol recovery from the discriminator (`nbfm_demod`, deviation 2400 Hz so ±3 symbols sit at
±1 as `fsk_demod` scales them), the sync words (LSF `0x55F7`, stream `0xFF5D`, packet `0x75FF`,
BERT `0xDF55`, EOT `0x555D`), the decorrelator, the interleaver, the K=5 rate-1/2 convolutional
code with its P1/P2/P3 puncturing (a Viterbi decoder), Golay(24,12) for the LICH, the LSF's
CRC-16 (poly `0x5935`, init `0xFFFF`), base-40 callsigns. The LSF arrives whole at stream start or
reassembled from six LICH chunks, so a receiver joining mid-stream has the callsigns within
240 ms. The M17 specification is public (the M17 Project, CC BY-SA); the MIT crate `m17core` may
serve as a test oracle or a dev-dependency for vectors, not as the implementation (builder's call,
under the rule that the stage decodes what the synthesiser of section 7.4 builds and what the
spec's example frames give).

### 7.2 Codec 2 as the external `c2dec`, and why

The TODO wanted Codec 2 in-crate. `deny.toml` forbids copyleft and CI runs `cargo deny`: the
pure-Rust `codec2` crate is LGPL-2.1 AND MIT and libcodec2 behind `codec2-sys` is LGPL-2.1, so
neither can enter the workspace without a licence exception (section 14, question 1). What can is
what the sondes and ADS-B do: a program on the host. `c2dec` from the `codec2` package (Debian and
Ubuntu, 1.2.0 on the field host) reads Codec 2 frames on stdin and writes 16-bit 8 kHz speech on
stdout (`c2dec 3200 - -`), one 8-byte frame to 160 samples at 3200 bps, one to 320 at 1600.

The stage names its vocoder through the row's `handlers` (section 4.2): `handlers: [{"programs":
["c2dec"], "install": {"debian": "codec2", "url": "https://github.com/drowe67/codec2"}}]`. A host
without it lists `m17_decode` `unavailable` with the report line `zeta: handlers: c2dec: missing
(apt install codec2; <url>) — modes: m17_decode`, the same way `adsb_decode` names dump1090. The
stage spawns `c2dec` per stream in the mode the LSF's TYPE field says (voice: 3200; voice and data:
1600, the 8 data bytes per frame dropped in this wave), feeds each stream frame's 16 payload bytes,
reads the speech, resamples 8 → 48 kHz (`Resampler`) and publishes it as the voice demodulators do
(48 kHz mono, `peak`, spans from the frames' times), so the page's player, the mix and
`job_attach --kind transcribe` see ordinary job audio (verified in the builder check:
`node_outputs` gives a terminal stage one job output per kind it emits, and `job_attach`'s
`last_audio` takes the job's last `audio` output with no kind gate). The process is restarted on the next LSF if
it exits; its exit fails nothing but the audio of that stream (the events keep coming), noted once
in `job_context.m17.vocoder`. The same stage shape (frames in, program, speech out) is the slot for
the AMBE/IMBE decoders of the vocoder gist: a handler the operator builds, never shipped.

### 7.3 Events, the row

Two events per stream, not one per 40 ms frame (the anti-goal): one when the LSF is in hand
(`frame` 0 or the frame number the LICH completed at), one at EOT. Fields: `protocol`, `src`, `dst`
(string; `ALL` for broadcast, the reserved `#` forms as the spec spells them), `can` (integer),
`type` (`voice`, `voice_data`, `data`, `packet`), `encryption` (null for none, else `scrambler`,
`aes`, `other`), `codec` (`3200`, `1600` or null), `frame` (integer), `eot` (boolean), `lat`, `lon`
(number or null: filled when the LSF META field carries the spec's GNSS form, else null; the
position map uses them). `summary`: `"{src} > {dst}: {type} {encryption?}"`. An encrypted stream
is reported with its fields and nothing is fed to the vocoder (section 11). A packet-mode
transmission is one event with `type: packet` and no content in this wave.

```
id m17_decode, name "M17 digital voice", family amateur_voice,
band {bandwidth_hz 12500, centers_hz [], min_hz null, range_hz null},
modulation ["fsk", "fm"], label "M17", protocol "m17",
decoder graph: channel → m17_decode, handlers [c2dec],
position {lat, lon, alt_m null},
identify {proof event, min_events 1, dwell_ms 5000, min_span_ms 1000, quick null, priority 0},
catalogue ["m17-rf-protocol"],
subject {key src, name "stations", waterfall fold,
         columns dst/Destination, type/Type, can/CAN, encryption/Encryption,
         heading null, label null, track_points 100, track_min_m 50},
notes "M17 (4FSK, 4800 baud, Codec 2) on any 2 m or 70 cm simplex or repeater channel, 12500 Hz
       band; start it on the measured centre. Its audio is ordinary job audio: listen to it or
       attach transcribe. Encrypted streams are reported, not decoded. It needs c2dec (package
       codec2) on the node's host."
```

The catalogue's `m17-rf-protocol` entry lists 29.85-30.15 MHz and 10.45-10.55 GHz, so its centre
term never helps on VHF; the row's empty `centers_hz` scores "anywhere" and the `fsk` label does
the rest. `quick: null`: `identify` tries it, `classify` does not (the attempt needs the runtime's
stream state; the row has no span-sized proof). `waterfall: fold`: a channel hears a few stations.

### 7.4 The synthesiser

Family `m17`: `{ "src": "VK4ZET", "dst": "ALL", "can": 0, "frames": 25 }`: the preamble, an LSF
frame (dst, src, TYPE voice 3200, META zero, CRC), `frames` stream frames with the LICH chunks
cycling, frame numbers counting, a zero payload (c2dec renders it as a low hum, which is audio
enough), the last with the EOT bit, then the EOT marker; convolutional coding, puncturing,
interleaving and decorrelation per the spec; 4FSK at 4800 baud, ±2400 Hz, root-raised-cosine
pulses (alpha 0.5; a 40-line modulator beside `gfsk_modulate`, or `gfsk_modulate` with order 4 if
the demodulator slices it as well: builder's call), 48 kHz. Validation: callsigns of at most nine
base-40 characters, `frames` 1-250. Expect `{src: "VK4ZET", dst: "ALL", type: "voice"}`,
`within_ms 5000`. About 400-600 lines; it needs no encoder for speech.

### 7.5 Size

`m17.rs` 700-1000, the stage with its vocoder pipe 250-350, section 4.2 100-150, section 4.3
60-100, the family 400-600, tests 300-400: about 1800-2600 lines, the largest item of the wave.
Section 13 says where it goes in the order.

## 8. DFM and M20 frame synthesis

Two families, `dfm` and `m20`, each `{ "serial": <as the decoder's id prints it>, "lat", "lon",
"alt_m", "vel_v", "frames" }`, each writing frames at the real line rate over NBFM at 48 kHz, each
proven only by its oracle on the field host: `zeta modes check --strict dfm_decode` through
`dfm09mod --json --ecc`, `m20_decode` through `m20mod --json`. The two rows get their `test`
blocks (`expect {id: <serial>, lat: {near, within 0.001}}`, `within_ms` long enough for the id,
which DFM spreads over several frames: 15 000 for DFM, 8 000 for M20, builder to tune).

The rule is the RS41 one and is written into each module's doc comment first, before any code,
the way `rs41_frame.rs` does: the public descriptions it follows, cited; which conventions the
descriptions leave open (bit order, CRC placement, the sync as sent) and how each was settled
(a published example frame, or the oracle); the decoder named as the oracle only. The builder
never opens `dfm09mod.c` or `m20mod.c`. Candidate descriptions, to be confirmed by the builder:
for DFM-06/09/17, the brmlab weather-sonde telemetry decoding page, the radiosonde_auto_rx wiki's
sonde notes and Graw's data sheets (the line: 2-FSK, Manchester coded, a sync word, Hamming-coded
nibbles, a configuration block carrying the serial across frames and two GPS blocks); for M20, the
same pages' M10/M20 notes and Meteomodem's data sheet (the line: 2-FSK at the M10 rate, a sync
header, one frame per second with a CRC-16). Anything a description does not settle is settled
against the oracle and said so.

Fallback, as plan 17 section 5.3: one task's budget per sonde. A generator the oracle still does not
decode at the end of it is dropped from the wave with its findings in the plan, and the row keeps
`test: null`. Whether a short public sample recording may stand in as a `fixture` test instead is
the operator's (section 14).

Size: 300-450 lines each, plus 60 for Manchester and Hamming(8,4) helpers.

## 9. APRS positions on the map

### 9.1 What exists

Everything but the parser. The APRS row has `subject.key: src`; the map draws a keyed row's fixes
per `${job}\u0000${key}` with the row's `track_points` and `track_min_m` (map.js `subjectEvent`,
plan 23 section 4.5, which named APRS 100 / 50 m); the Subjects table shows a position column and
the distance from home. The TODO's dependency on "the map's per-(job, key) tracks from the ADS-B
display gist" is met.

### 9.2 The parser

`zeta-dsp/src/aprs_info.rs`, `parse_info(dst: &str, info: &[u8]) -> AprsInfo`, from the APRS 1.0.1
specification (public) and its examples:

- uncompressed position reports (`!`, `=`, and `/`, `@` with a 7-character timestamp): latitude
  `ddmm.hhN`, longitude `dddmm.hhW`, the symbol table and code, position ambiguity (spaces replace
  digits: the fix is the centre of the ambiguous cell and `ambiguity` counts the digits), then
  `ccc/sss` course and speed when present, `/A=aaaaaa` altitude in feet anywhere in the comment;
- compressed positions (Base-91 latitude, longitude, symbol, and the course/speed, altitude or
  radio-range bytes by the compression type);
- Mic-E (the destination address carries the latitude digits, the message bits, N/S, the
  longitude offset and E/W; the first eight info bytes the longitude, speed, course and symbol).

Objects (`;`) and items (`)`) are recognised as `data_type` with the object's name in `object`, but
their coordinates are not emitted as `lat`/`lon` in this wave: the subject key is the sender, and an
object's fix on the sender's track would be wrong (a leftover, section 13). Messages, status,
telemetry and weather set `data_type` and nothing else new.

New event fields: `lat`, `lon`, `alt_m` (number or null; feet converted), `course_deg`, `speed_kt`
(number or null), `symbol` (two characters or null), `comment` (string or null), `data_type`
(`position`, `mic_e`, `object`, `item`, `message`, `status`, `telemetry`, `weather`, `other`),
`object` (string or null). `src`, `dst`, `path`, `info`, `info_hex`, `fcs_ok` and the summary
`"{src} > {dst}: {info}"` are unchanged, so the page's pinned text and every consumer stand.

The `kiss` parser in `zeta-ext` calls the same function, so a KISS TNC row gets positions too.

### 9.3 The row

`position: {lat, lon, alt_m: "alt_m"}`, `subject.heading: "course_deg"`, `track_points 100`,
`track_min_m 50` (plan 23's numbers, never written into the row until now). The test's `info`
becomes a position, `"!2723.05S/15307.05E>ZetaSDR"` (Brisbane Airport to the format's 0.01'),
with `expect` gaining `lat: {near: -27.3842, within: 0.0001}, lon: {near: 153.1175, within:
0.0001}`; a second test variant is not needed, the echo stays.

### 9.4 Size

Parser 300-400 with unit tests on the specification's examples for the three formats (150), stage
and `kiss` integration 50, row and e2e 80: about 600-750 lines.

## 10. The rows at a glance

| Row | Family | Band | Hook | Catalogue `decoder` | Prose | Size (lines) |
|---|---|---|---|---|---|---|
| `ais_decode` (graph, new) | maritime | 25 000 Hz at 161 975 000 / 162 025 000 | `event`, `quick: ais`, dwell 10 s | `ais`, `automatic-identification-system-ais` | row `notes`; `note("job_start")` one sentence; field craft "Decoding pagers, aircraft and FM data" bullet and "Where to look" | 1100-1450 |
| `flex_decode` (program, new) | pagers | 25 000 Hz, no centres | `event`, `quick: null`, dwell 15 s | `flex` | as above; the handler paragraph names multimon-ng | 700-1000 |
| `m17_decode` (graph + handler, new) | amateur_voice | 12 500 Hz, no centres | `event`, `quick: null`, dwell 5 s | `m17-rf-protocol` | row `notes`; `note("job_start")` and `note("job_attach")`; field craft: a "Decoding digital voice" paragraph under "Transcribing a channel" | 1800-2600 |
| `dfm_decode`, `m20_decode` (tests only) | weather_science | unchanged | unchanged | unchanged | field craft's "no synthesised test yet" sentence goes | 700-1000 |
| `aprs_decode` (fields + position) | amateur_digital | unchanged | unchanged | unchanged | "Decoding APRS" gains the position sentence; row `notes` added | 600-750 |
| shared (section 4) | | | | | `note("modes_list")` (`outputs`), `note("classify")` (five quick names), the modes guide (row notes) | 300-450 |

Whole wave: about 5 200-7 250 lines including tests and generators. The prose locations above are
all generated-and-checked text (`agents_md.rs`, `field-craft.md`, the rows' `notes`); `README.md`'s
"Legal" paragraph and `docs/guide.md`'s decoder list are the hand-written ones.

## 11. Legal and field craft

- **AIS and APRS** are broadcasts meant to be received by anyone (AIS is a collision-avoidance
  system; APRS is amateur traffic). Nothing new to say; field craft's list of "ordinary use" rows
  ("A known public protocol the node has a decoder row for (APRS, POCSAG, ADS-B, ACARS,
  radiosondes) is the ordinary use of the radio") gains AIS, FLEX and M17.
- **FLEX follows POCSAG exactly.** The POCSAG row decodes every page it hears, keeps no address
  list, applies no filter, and the content lands where every event lands (`events.log`, the page's
  ribbons, `--then-tail`). The guard is the README's "Legal" paragraph ("Whether you may receive and
  decode a given transmission where you are, pager traffic in particular, depends on your local law
  and is your responsibility") and the field-craft rule that the agent states the legal line once
  when a request goes that way. FLEX does the same: the README paragraph names FLEX beside POCSAG,
  and no redaction or retention switch is added without the operator's word (section 14).
- **M17** is amateur voice, public by licence condition. An encrypted stream (`encryption` not
  null) is reported with its callsigns and type and is never fed to the vocoder: the layer-4
  boundary ("never attempts to break encryption") applies as written. Packet-mode content is not
  read in this wave; when it is, it is the same public amateur traffic.
- **Nothing transmits and nothing leaves.** The AIS `nmea` field is a value in an event; no sink
  sends it anywhere.
- `README.md` "Legal" lists the decoders: AIS, FLEX and M17 join the sentence.

## 12. Testing

Unit (each in its crate):

- `ais_msg`: the published sentence in `nmea.rs`'s test (`!AIVDM,1,1,,B,177KQJ5000G?tO`K>RA1wUbN0TKH,0*5C`,
  MMSI 477553000, type 1) decodes to the full fields; a type 5 and a type 18 vector from the
  ITU-R M.1371 examples or a public AIS test set; `encode_sentences` of a decoded payload gives the
  original sentence back; the bit-reversal self-check: a frame built by the `ais` family, deframed
  by `HdlcDeframer`, decodes to the parameters it was built from.
- `ais_decode` stage: the synthesised frame through the stage emits the event; a rate change and a
  non-IQ input are rejected (as the APRS stage's tests do); the quick attempt proves on a buffered
  span and reports `no_frame` on noise.
- `multimon` parser: both FLEX line forms give the fields; a FLEX line that does not parse stays
  `{text}`; POCSAG lines unchanged.
- `m17`: Golay(24,12) encode/decode with the spec's vectors; the Viterbi decoder recovers a punctured
  codeword with two bit errors; the LSF CRC matches a spec vector; a frame built by the `m17` family
  decodes to its callsigns; the stage emits two events for a stream and 48 kHz audio frames whose
  span count matches the frames fed (with a fake `c2dec` that writes silence, the way
  `fake_dump1090` stands in for dump1090).
- `aprs_info`: the three formats' examples from the specification, ambiguity, `/A=`, course/speed,
  an object (named, no fix), a message (no fix).
- `rs41_frame`-style unit tests for the `dfm`, `m20`, `flex` and `m17` generators: CRCs and codes
  checked with the in-tree encoders, never the decoders.
- `compare`: the tolerance form passes within, fails outside, and a non-number fails with the diff
  line.
- Rows: `crates/zeta-core/tests/modes.rs` gains the three new rows as `ready` on a host with their
  programs and `unavailable` without (`ZETA_HANDLER_C2DEC=/nonexistent`), `modes_list` `outputs`
  for `m17_decode` and `aprs_decode`, a graph row with a bad `handlers` entry `broken` naming
  `decoder.handlers[0].programs`; its `EMBEDDED` list grows from eight to eleven and
  `the_rows_thin_their_tracks_as_planned` moves `aprs_decode` from `(100, 0.0)` to `(100, 50.0)`
  and adds the AIS and M17 rows.

`zeta modes check`:

- CI: `modes_check_passes_the_four_in_crate_rows` becomes the in-crate rows, `ais_decode` included;
  `aprs_decode`'s check now proves the position. CI's runner installs `codec2` and `multimon-ng`
  (apt; both are in Ubuntu), so `--strict m17_decode flex_decode` runs there too and the
  synthesisers are proven on every push, not only on the field host. The strict step for a row
  lands in the same task as that row's passing test, never before it, so CI is never red between
  landings; if a row takes the `test: null` fallback its id is left out of the strict step and the
  plan says so. The sonde decoders are not packaged for Ubuntu, so `dfm_decode`, `m20_decode` and
  `rs41_decode` stay `skipped` in CI and `--strict` on the field host is their gate.
- Field host: `zeta modes check --strict` passes every embedded row (the acceptance line).

Page (`ui.py`, one scenario within the budget): `scenario_map_tracks_an_aprs_station`: an APRS
fixture whose packet carries a position (the e2e harness writes its fixture inline in
`crates/zeta/tests/ui.rs` with `aprs_packet_iq("VK3ABC", "APRS", ">hi", ...)`, as `cli.rs` and
`modes_check.rs` do; that call's `info` becomes the row's position info, or a second inline fixture
is added) gives one subject marker on the map and a row in the Subjects table with the position
column filled; `scenario_aprs_ribbon_appears` asserts the roster line (`aprs 1 stations, last
VK3ABC`), so it passes either way. The M17 play and Transcribe buttons are covered by a unit-level check of the
`AUDIO_KINDS`-or-row rule rather than a scenario (no M17 fixture plays in the suite).

Live acceptance (`UAT.md` Plan 29, the operator's):

- AIS: on the field host's B210 with the discone, `plan_start ais_vhf` (or two `ais_decode` jobs),
  Moreton Bay and Port of Brisbane traffic: vessels appear in the Subjects tab with names within a
  few minutes, markers move on the map, `summary` lines read `<name> <lat>,<lon> <speed>`; `classify`
  on the channel lists `ais_decode proved`.
- APRS: `aprs_decode` on 144 800 000 Hz: stations with positions get markers; a Mic-E tracker
  (common on 2 m) decodes to a fix.
- FLEX and M17: section 14 asks what is in range; without a live signal the synthesised `--strict`
  check is the proof and the UAT entry says so.
- DFM/M20: `--strict` passes against `dfm09mod` and `m20mod`; a live DFM or M20 is not expected
  over Brisbane (the Bureau flies RS41) and the watch's existing rows handle one if it comes.

## 13. Left to the plan and the builder

Order of work (the plan's), by value and by what unblocks what: section 4 first (the tolerance,
handlers, `outputs`, notes), then APRS positions (smallest, visible at once on the field host),
AIS, DFM and M20 synthesis, FLEX, M17 last; M17 is the largest item and may be its own plan (28b)
if the wave's reviews run long. Each row lands with its row, stage, generator, tests and prose in
one task cluster, as the sonde rows did.

Builder's calls, bounded by the tests above:

- `gfsk_modulate`'s `bt` argument (AIS 0.4) and whether M17's modulator is RRC or reuses Gaussian.
- The AIS bit clock (the AFSK stage's transition-pulled clock, or the slicer's Gardner loop as a
  library call) and the discriminator's low-pass.
- Where the BCH(31,21) encoder lives once FLEX shares it with POCSAG.
- Whether `LoadedMode` also records the handlers' resolved paths for `modes_show`, and whether
  `probe_handlers` groups a graph row's handlers with program rows that name the same program
  (`c2dec` has none today).
- `c2dec`'s exact invocation for 1600 and the restart policy on exit; whether `--softdec` input
  helps (it does not: the frames are hard bytes).
- The DFM and M20 `within_ms`, the serial formats and which blocks may stay zero.
- Whether `ais_vhf` enters `plans.rs` in this wave.

Leftovers to write into `TODO.md` when the plan lands: APRS objects and items as subjects of their
own; an in-crate FLEX decoder (the slot); M17 packet-mode content and the 1600 mode's data bytes;
AIS message types beyond 1-5, 18, 21, 24; a `vocoder` handler for AMBE/IMBE on the same stage
shape (ties to the install-script gist); AIS and APRS feeds out (the Feeds gist); the FLEX
synthesiser at 3200 and 6400 bps.

## 14. Questions for the operator

1. **Codec 2 and the licence rule.** `deny.toml` forbids copyleft and CI enforces it. The pure-Rust
   `codec2` crate is LGPL-2.1 AND MIT and libcodec2 is LGPL-2.1. This document runs Codec 2 as the
   external `c2dec` program (package `codec2`, already on the field host), so M17 audio needs a
   program the way ADS-B needs dump1090. Do you want that, or an LGPL-2.1 exception in `deny.toml`
   for the `codec2` crate so M17 decodes to audio with nothing installed?
2. **FLEX in range.** Do you know a live FLEX channel reachable from the field host or the desk
   (a centre frequency), or is FLEX synthesised-only for now? The answer sets the row's
   `centers_hz` and whether the UAT has a live line.
3. **M17 on air.** Do you have, or can you borrow, an M17-capable transmitter (an OpenRTX radio, a
   Module17) for an on-air check, or is the synthesised `--strict` check the acceptance for this
   wave?
4. **A recording as a fixture.** If the public descriptions of DFM or M20 prove insufficient to
   synthesise a frame the oracle decodes within the task budget, may a short public sample
   recording (a few seconds, its licence recorded) enter the repo as a `fixture` test for that row,
   or does the RS41 rule ("no recording in the repo") hold and the row keeps `test: null`?
5. **Pager content on disk.** POCSAG pages already land in `events.log` and the page's ribbons;
   FLEX pages will too, under the same README paragraph and nothing else. Keep that, or do you want a
   retention or redaction switch for pager text before FLEX lands?

## 15. Calls made in this document (check these while reading)

- AIS is in-crate, FLEX is multimon-ng, Codec 2 is `c2dec`: by what exists on the host and what
  the licence rule allows, not by preference; each has its slot for the other shape.
- One stage per row from IQ to its outputs; no inner event edges (plan 27's feed would carry
  them).
- `mmsi` is an integer, as the `nmea` parser already emits it; the subjects store keys numbers by
  their text.
- AIS `waterfall: summary`, M17 `fold`, by the number of subjects a job hears (plan 23's rule).
- AIS `dwell_ms` 10 000 covers a vessel under way, not at anchor; the notes say so rather than
  the dwell growing to three minutes.
- AIS type 24 is read beside 1-5, 18, 21 so class B vessels get names.
- The AIS `nmea` field is an array of sentences (a type 5 spans two).
- FLEX synthesis is 1600 bps 2-level only; 3200 and 6400 are a leftover.
- FLEX has no subject and no filter, as POCSAG; the README paragraph is the guard (section 14
  asks before anything more).
- M17 emits two events per stream (LSF and EOT), not one per frame.
- A missing `c2dec` makes `m17_decode` `unavailable` rather than a ready row without audio: one
  rule for missing programs, and the package is small.
- The M17 stage lives in `zeta-dsp`, not `zeta-ext`, because an embedded graph row may name only
  stages `zeta_dsp::register_all` registers (the core tests prove the rows with that registry
  alone); `find_handler` moves to `zeta-core` so the probe and the stage share one lookup.
- A row's strict CI step lands with its passing test, and a row on the `test: null` fallback is
  left out of it.
- M17 `lat`/`lon` come from the LSF META GNSS form when present; the row has a `position` map
  because of it.
- APRS objects and items are recognised but not positioned, since the key is the sender.
- The APRS summary and the existing fields are unchanged, so no pinned page text moves.
- `expect` tolerances are `{"near", "within"}` on a field, nothing more elaborate.
- CI installs `codec2` and `multimon-ng` so the M17 and FLEX synthesisers are proven on every
  push; the sonde decoders stay field-host-only.
- Row `notes` are rendered into AGENTS.md (the row schema promised it; nothing did it).
- M17 is last in the order and may split into its own plan.
