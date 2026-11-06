# Plan 30, aircraft data links: the VDL Mode 2 and HFDL rows

Design spec, 2026-11-06. From two operator notes in `TODO.md`: "Modes: VDL Mode 2 beside ACARS,
and an airband capture plan" (2026-11-05) and "Feature: VDL Mode 2 and HFDL rows (the other
aircraft data links)" (2026-11-02). A reviewer read it as the builder before hand-over, and the
operator answered its questions the same day: section 7 records the decisions, section 8 the
calls made here. Builds on the mode registry
(`docs/superpowers/specs/2026-10-27-mode-registry-design.md`, `crates/zeta-core/src/modes/`),
the external-program adaptor (`crates/zeta-ext/src/external.rs`), `zeta modes check`
(`crates/zeta/src/modes_check.rs`) and subjects (`docs/superpowers/specs/2026-11-02-subjects-design.md`,
`crates/zeta-ui/web/subjects.js`). The map layer (plan 32,
`docs/superpowers/specs/2026-11-06-map-layer-design.md`) draws from the source position this
plan adds; it does not design the map side.

## 1. Intent and why

Plain VHF ACARS is `acars_decode`, and the field host runs seven of them on the B210. Most
airline traffic has moved on: to VDL Mode 2 on VHF (136.975 MHz worldwide, D8PSK at 10.5 kbaud
in a 25 kHz channel, ACARS carried over AVLC) and, over oceans and outback, to HFDL (ARINC 635,
PSK at 1800 baud in a 2.4 kHz USB channel on a dozen HF frequencies). The catalogue knows both
(`vhf-data-link-mode-2-vdl-m2`, `high-frequency-data-link-hfdl`) with `decoder` null, so
`knowledge_lookup` names the signal and offers nothing to prove it. The register has SITA's
assignments on 136.975 MHz at Brisbane Airport; VDL2 there is busy.

The decoders exist and are installed on the field host: `dumpvdl2` 2.6.0 and `dumphfdl` 1.7.0,
both over `libacars` 2.2.1. The program-row machinery of plan 16 fits them almost as it stands:
IQ over stdin at the rate the program asks for, one JSON object per line back. Two gaps stop a
row today, and both are generic rather than VDL2's:

1. `json_lines` takes the line's top-level object as the event. dumpvdl2 nests everything under
   `vdl2.avlc...` (the ACARS payload under `vdl2.avlc.acars`), dumphfdl under `hfdl.lpdu...`,
   so `summary`, `subject`, `position` and `time` cannot name their fields. Any program that
   prints structured JSON has this problem; rtl_433 and direwolf's JSON do not, which is why it
   has not bitten yet.
2. `dumphfdl` takes its frequencies in kilohertz and refuses hertz (`'8942000': value too
   large`, checked on the field host); the only frequency placeholder is `$center_hz`.

This plan closes both gaps in the row schema and adds two rows, `vdl2_decode` and `hfdl_decode`.
What the operator wants from them (section 7, decision 4) is aircraft on the map wherever the
link carries a position (ADS-C reports libacars decodes inside dumpvdl2; HFDL's structured
`pos`), with the same details ACARS and ADS-B give (`reg`, `flight`, `label`, `text`) and a link
to the job, and one aircraft table on the page across ACARS and VDL2 (decision 20). Beside the
rows, two small generic pieces: a per-source position (a KiwiSDR knows where it is; decision
10) and a page-side merge of subjects across rows (section 4.8). The node's job machinery does
not change: a row is data, and the agent, the page and the CLI get both modes the way they got
the sondes. The fixtures are the one novelty: neither mode has a synthesiser, so each row's
check runs on a short capture recorded off the air.

## 2. Goals, non-goals, anti-goals

### Goals

- A program row can say where in its program's JSON each event field lives, so a nested output
  becomes a flat event with the row's own field names, and `summary`, `subject`, `position` and
  `time` work as on every other row.
- `vdl2_decode`: dumpvdl2 on one 25 kHz channel, events keyed on the AVLC source address with
  the ACARS fields as columns and a `position` on the frames that carry an ADS-C report,
  started like any decoder (`job_start --kind vdl2_decode`, `identify`, the page's decode menu),
  beside the ACARS jobs on the same radio.
- `hfdl_decode`: dumphfdl on one HFDL channel of a KiwiSDR (or any HF source), the same shape,
  `position` from the HFDL position PDUs.
- Both rows carry a check: `zeta modes check --strict vdl2_decode hfdl_decode` passes on a host
  that has the programs, against fixtures in the repository; on a host without them it says
  `skipped: dumpvdl2 missing`, as `adsb_decode` does without dump1090.
- Both catalogue entries gain their `decoder`, so `knowledge_lookup` at 136.975 MHz answers
  `vdl2_decode`.
- The page shows one aircraft table across `acars_decode` and `vdl2_decode` (and `hfdl_decode`),
  an aircraft heard on two links being one row, merged by `reg` or by ICAO address.
- A source carries its own position (`source_list`): a KiwiSDR's from its status page where the
  Kiwi has one, else a configured one; the station stays the node's. `hfdl_decode` events say
  which source heard them, so the map layer can draw them from the receiver, not the station.
- The agent guide says how to run VDL2 beside ACARS on one radio, and how to find an HFDL
  channel on a Kiwi.

### Non-goals

- Positions parsed out of free ACARS message texts (some airline texts carry one). The
  positions this plan maps are structured fields libacars and dumphfdl decode: ADS-C over
  ARINC 622 inside the ACARS block, and HFDL's `pos`.
- Multi-channel decoding. dumpvdl2 and dumphfdl each decode several channels from one wide
  stream; a row decodes one channel per job, as every row does. Every-channel-in-the-capture
  decoding is the span-decoder work (plan 31, the sibling spec on span decoding and named sets).
- The CPDLC, CM and X.25/ATN payloads dumpvdl2 and dumphfdl decode (ADS-C over the ATN half
  included: its JSON is a different, deeper tree). The rows carry the AVLC/LPDU identity, the
  ACARS block and the ARINC 622 ADS-C basic report; the rest stays in the program's line and is
  not an event field in this plan (a later row revision can map more; nothing here prevents it).
- Ground-station tables, `--gs-file`, `--system-table`, `--station-id`, feeds to airframes.io
  or similar. The rows run the programs bare.
- The "airband" named set: one call that starts ACARS + VDL2 on one radio and ADS-B on another,
  as `plan_start` does for a channel plan. That is plan 31, not this one. This plan gives it the
  rows it will start.
- The map side of the source position: what the Map tab draws for a positioned source, bearings
  from it, cutting the register's sites by its reach. Plan 32 does that, and also owns the field
  itself (`SourceInfo.position`, section 4.5).
- Wiring the operator's own KiwiSDR into the deployed node. The operator adds it to their
  instance themselves (decision 7); no task, deploy helper, unit file or persisted state here
  does. This plan makes sure a Kiwi they add works: `hfdl_decode` on a `--source-kiwi` source,
  and the source's position.
- Patching the vendored catalogue's HFDL frequency ranges (decision 21: left as upstream).

### Anti-goals

- A field map that only VDL2 can use. Pointers into a parsed line are generic; nothing in the
  schema or the adaptor knows dumpvdl2's names.
- A merge rule the page hard-codes for two rows. The merge is declared on the rows
  (`subject.merge`) and the page applies it to any rows that declare it.
- A fixture that is large or that carries anything private. The repository is public; each
  fixture is small, reviewed line by line, and holds only link-layer and operational traffic.
- A row that passes its check on the field host and nowhere else without saying why. A missing
  program or a missing fixture is a `skipped` with the reason, and `--strict` fails it.
- A row that works only when the capture is wide enough by luck. The rows state their channel
  width and rate; the node's channeliser and the adaptor's resampler do the rest, bank or no
  bank.
- The local KiwiSDR's host name in a tracked file. It is "the local KiwiSDR" in prose and
  `kiwi-host` in URLs, paths and flags; the real name lives in untracked configuration and a
  developer shell's environment only.

## 3. Constraints

- A row is `deny_unknown_fields`, every field required unless `#[serde(default)]`; errors name
  `<field>: <error>` (`RowError`), and a bad row is `broken`, never a dead node
  (`crates/zeta-core/src/modes/row.rs`). AGENTS.md is generated; prose goes in
  `crates/zeta/src/agents_md.rs` or `docs/agents/field-craft.md`.
- A program row expands to `channel → external_decode` (`modes/template.rs`). The channel
  decimates by an exact integer to `channel_rate_hz` (`ops.rs`): the narrowest exact divisor of
  the source (or bank bin) rate that is at least `max(bandwidth_hz, 48 000)`. The adaptor's
  `Converter` then resamples rationally to `input.rate_hz` (`external.rs`, `Resampler`), so the
  program's rate need not divide anything; a change of input rate restarts the resampler.
- `$center_hz` is the first IQ frame's centre: the channel's, since the channel node mixes the
  job band to baseband. Placeholders are the fixed list `PROGRAM_PLACEHOLDERS` (`$port`,
  `$rate_hz`, `$center_hz`, `$fifo`; `row.rs`), substituted by `substitute` in `external.rs`; an
  unknown one is `broken` at `decoder.args[i]`.
- The publisher (`external.rs`, `Publisher::publish`) parses each line with the row's parser
  (`crate::parsers::parse`), then `summary::envelope` fills `protocol`, `summary`, `t_ns` and
  `position` from the flat object's top-level keys (`position` needs numbers at the mapped
  `lat`/`lon`; `alt_unit: ft` converts). `time` must be an ISO 8601 UTC string.
- A subject's `key`, `columns[].field`, `heading`, `label`, the `position` map and the summary
  template must all name `event.schema.properties`, else `broken` naming the field.
- `zeta modes check` opens a node on a file source over the test's SigMF pair; a `fixture`
  test's path is read as given (today: relative to the current directory) and the job centre is
  the meta's `captures[0].core:frequency`. Skips are `no test` and `<program> missing`;
  `--strict` turns every skip into a fail. It takes one id (`ModesCmd::Check { id: Option<String> }`),
  and waits for the first event with one `stream_tail` capped at `TAIL_MAX_MS` (10 s). The file
  source reads `cf32_le`, `ci16_le` and `ci8`.
- `.gitignore` ignores `*.sigmf-data` repository-wide; the OOK fixtures' data files under
  `crates/zeta-dsp/tests/data/` are tracked because they were force-added. A new fixture
  directory needs an un-ignore rule (`!/modes/fixtures/*.sigmf-data`) or the same force-add,
  and the plan says which.
- The e2e suite (`crates/zeta/tests/ui.rs`, `crates/zeta-ui/tests/e2e/ui.py`) runs `zeta serve`
  with a `--modes-dir` of fake `json_lines` rows (`fake_pos`, `fake_subjects`, `fake_fold`)
  whose program is zeta-ext's `fake_lines` with `--lines-file`: that is how a page scenario
  gets decoder events without a decoder. The subjects scenarios (`scenario_subjects_*`) read
  `#subjects section.subject-job` and `tr.subject[data-key]`.
- A KiwiSDR source (`crates/zeta-source-kiwi`) delivers 12 kHz of IQ, blocks stamped at the
  nominal 12 000 Hz; the true rate is a little over (`reported_sample_rate`). `bandwidth_hz` is
  at most 12 000. The crate speaks the Kiwi websocket protocol over `tokio-tungstenite`
  (`session.rs`) and has no HTTP client; `Capabilities` (`crates/zeta-core/src/types.rs`) has
  `remote: bool` and no position; `Port` (`port.rs`) has defaulted accessors `fault`, `gain`,
  `stats`, `worker`, and `source_list` (`ops.rs`, `Op::SourceList`) fills `SourceInfo` from
  them. `--source-kiwi ID=HOST:PORT[/PASSWORD]` is parsed by `parse_kiwi` (`crates/zeta/src/sources.rs`).
- The B210 capture at 8 MHz gets a polyphase bank of 8 bins (1 MHz spacing, 2 MHz per bin,
  `pfb.rs`); a channel wider than a bin (`TooWide`) is filtered directly from the capture.
  The lease cannot widen a capture under a running rigid job ("widen capture ... not yet
  available", `lease.rs`): a band outside the capture is a conflict naming the holders.
- Checked on the field host (dumpvdl2 2.6.0, dumphfdl 1.7.0, zero-filled input):
  - dumpvdl2's `--sample-format` accepts `U8` and `S16_LE`; `S16LE`, which its `--help`
    prints, and `CS16` are refused as `Unknown sample format`. `--oversample 1` runs
    ("Sampling rate set to 105000 sps"), as does 2 (210 000).
  - dumphfdl runs at `--sample-rate 12000` with `CS16` input, takes frequencies in kHz only,
    and without `--centerfreq` computes the centre from the channel list ("computed center
    frequency: 8942.000 kHz" for one channel).
  - Neither program, nor libacars, is in Debian or Ubuntu. The field host has them from the
    OpenWebRX+ apt repository (`dumpvdl2`, `dumphfdl`, `libacars2`); upstream is
    `github.com/szpajder/{dumpvdl2,dumphfdl,libacars}`.
  - JSON key names seen in the binaries: dumpvdl2 `vdl2`, `app`, `avlc`, `src`, `dst`,
    `frame_type`, `xid`, `x25`, `sig_level`, `noise_level`, `Aircraft`, `Ground station`,
    `Airborne`, `On ground`; libacars `reg`, `flight`, `msg_text`, `crc_ok`, `err`, `more`,
    `blk_id`, `msg_num`, `msg_num_seq`, `sublabel`, `mfi`; dumphfdl `hfdl`, `lpdu`, `spdu`,
    `mpdu`, `hfnpdu`, `ac_info`, `icao`, `flight_id`, `pos`, `lat`, `lon`, `bit_rate`,
    `freq_skew`, `gs_status`.
  - Read in libacars's source (`acars.c`, `arinc.c`, `adsc.c`, 2026-11-06): the ACARS node's
    key is `acars` with `err`, `crc_ok`, `more`, `reg`, `mode`, `label`, `blk_id`, `ack`,
    `flight`, `msg_num`, `msg_num_seq` (downlinks), `sublabel`, `mfi` (when set), `msg_text`;
    its child for labels A6, AA, B6, BA and H1 is `arinc622` (`msg_type` `adsc_msg`, `crc_ok`,
    `gs_addr`, `air_addr`), whose child is `adsc`: a `tags` array of one-key objects
    (`basic_report`, `emerg_basic_report`, `flight_id`, `predicted_route`, `earth_ref_data`,
    `air_ref_data`, `meteo_data`, `airframe_id`, `intermediate_projection`, `fixed_projection`,
    the event reports, ...) and `err`. `basic_report` (and `emerg_basic_report` and the event
    reports, which share its formatter) carries `lat`, `lon` (degrees, doubles), `alt`
    (integer feet), `ts_sec` (seconds past the hour), `pos_accuracy_nm`, `nav_redundancy`,
    `tcas_avail`. The tree-to-JSON nesting (child under parent) is read from the node links,
    not from a captured line: the exact depth and the tag wrapper are confirmed on the first
    real line (section 6).
- The repository is public: fixtures and row notes use roles, not host names or positions
  (the demo station is Brisbane Airport). `scripts/public-check.sh` must stay clean.

## 4. The design

### 4.1 `output.fields`: event field ← JSON pointer

A program row's `decoder.output` gains one optional map:

```json
"output": {
  "via": "stdout", "port": null, "parser": "json_lines",
  "fields": {
    "icao":   "/vdl2/avlc/src/addr",
    "reg":    "/vdl2/avlc/acars/reg",
    "lat":    ["/vdl2/avlc/acars/arinc622/adsc/tags/0/basic_report/lat",
               "/vdl2/avlc/acars/arinc622/adsc/tags/1/basic_report/lat"]
  }
}
```

- A key is an event field name; it must be a property of `event.schema`, else `broken` naming
  `decoder.output.fields.<key>`. Every schema property must be a key of the map when the map is
  present, else `broken` naming `event.schema.properties.<name>` ("not mapped by
  `decoder.output.fields`"): with a map, the event holds the mapped fields and nothing else, so
  an unmapped property would be a field that never arrives. The envelope's own fields
  (`protocol`, `summary`, `t_ns`, `position`), which `envelope` adds after the map and which
  existing rows list in their schema, are exempt from that rule.
- A value is one JSON pointer (RFC 6901, `serde_json::Value::pointer`; must start with `/`,
  else `broken`) or a non-empty list of them, the first that resolves to a non-null value
  winning (the `{a?b}` rule of summaries, applied at the source). A pointer that resolves to
  nothing, or to JSON `null`, leaves the field out of the event; `{field?}` in the summary then
  drops it, a subject key that is absent makes the event "about no subject" (a ribbon, no table
  row), as `row.rs` already defines for a null key, and a `position` whose `lat` or `lon` is
  absent is no position (`envelope` already does this).
- The value is copied as it is: numbers stay numbers, strings strings; an object or array is
  copied too (nothing in this plan needs one, and refusing them would gain nothing).
- Applied in `Publisher::publish` between the parser and `envelope`, for every parser: SBS,
  NMEA and the rest produce flat objects, on which `/icao` is the same as today's `icao`, so
  no existing row changes. Without `fields`, behaviour is exactly today's (the whole object).
- Validation lives in `validate_program` beside the placeholder checks; `modes_list` and
  `modes_show` report the map as part of the row, as they report everything.

**Why pointers, not flattening.** Flattening (`vdl2.avlc.acars.reg` as the field name) keeps
every consumer reading the decoder's nesting: the schema, the summary template, the subject
columns and the page's column labels would all spell `vdl2.avlc.acars.reg`, and a dumpvdl2 line
flattens to some sixty keys (`app`, `burst`, `xid` parameters), of which the page and the agent
want ten. A map names what the row promises, with the names the row chooses (`reg`, `flight`,
`label`, `text`: the same words `acars_decode` uses, so the two aircraft tables read alike, and
so the page can merge them, 4.8). It costs no dependency (`serde_json` has pointers), handles
keys with dots and slashes by the standard's escaping, and the list form gives "src or dst" and
"the first tag that is a basic report" without inventing syntax. It is also what a later
`time`-from-`{sec, usec}` or a text parser would plug into: the event is a flat object with
known names once the map has run.

### 4.2 `$center_khz`

A fifth placeholder, `$center_khz`: the centre in kilohertz, an integer when it divides, else
with up to three decimals (`8942`, `8942.5`, `136975`). Added to `PROGRAM_PLACEHOLDERS`, to
`substitute`, and to the `args` doc text. It exists because dumphfdl wants kHz; it is generic
(several HF decoders take kHz).

### 4.3 `vdl2_decode`

```json
{
  "id": "vdl2_decode",
  "name": "VDL Mode 2 aircraft data link",
  "family": "aviation",
  "band": {"bandwidth_hz": 25000, "centers_hz": [136975000, 136725000, 136775000, 136825000, 136875000], "min_hz": null, "range_hz": null},
  "modulation": ["psk"],
  "label": "VDL2",
  "protocol": "vdl2",
  "decoder": {
    "kind": "program",
    "programs": ["dumpvdl2"],
    "args": ["--iq-file", "-", "--sample-format", "S16_LE", "--oversample", "1",
             "--centerfreq", "$center_hz", "--output", "decoded:json:file:path=-", "$center_hz"],
    "input": {"stream": "iq", "format": "cs16", "rate_hz": 105000, "via": "stdin"},
    "output": {"via": "stdout", "port": null, "parser": "json_lines",
      "fields": {
        "icao": "/vdl2/avlc/src/addr", "addr_type": "/vdl2/avlc/src/type", "status": "/vdl2/avlc/src/status",
        "dst": "/vdl2/avlc/dst/addr", "frame_type": "/vdl2/avlc/frame_type",
        "reg": "/vdl2/avlc/acars/reg", "flight": "/vdl2/avlc/acars/flight", "label": "/vdl2/avlc/acars/label",
        "msg_num": "/vdl2/avlc/acars/msg_num", "ack": "/vdl2/avlc/acars/ack", "text": "/vdl2/avlc/acars/msg_text",
        "crc_ok": "/vdl2/avlc/acars/crc_ok",
        "lat": ["/vdl2/avlc/acars/arinc622/adsc/tags/0/basic_report/lat", "/vdl2/avlc/acars/arinc622/adsc/tags/1/basic_report/lat", "/vdl2/avlc/acars/arinc622/adsc/tags/0/emerg_basic_report/lat"],
        "lon": ["/vdl2/avlc/acars/arinc622/adsc/tags/0/basic_report/lon", "/vdl2/avlc/acars/arinc622/adsc/tags/1/basic_report/lon", "/vdl2/avlc/acars/arinc622/adsc/tags/0/emerg_basic_report/lon"],
        "alt_ft": ["/vdl2/avlc/acars/arinc622/adsc/tags/0/basic_report/alt", "/vdl2/avlc/acars/arinc622/adsc/tags/1/basic_report/alt", "/vdl2/avlc/acars/arinc622/adsc/tags/0/emerg_basic_report/alt"],
        "sig_level": "/vdl2/burst/sig_level", "noise_level": "/vdl2/burst/noise_level"
      }},
    "install": {"debian": null, "url": "https://github.com/szpajder/dumpvdl2"}
  },
  "event": {"schema": {"type": "object", "properties": {"icao": {"type": "string"}, "...": "one per mapped field; the ACARS and ADS-C ones nullable"}},
            "summary": "{icao} {reg?} {flight?} {label?} {text?}"},
  "identify": {"proof": "event", "min_events": 1, "dwell_ms": 15000, "min_span_ms": 1000, "quick": null},
  "catalogue": ["vhf-data-link-mode-2-vdl-m2"],
  "test": {"kind": "fixture", "path": "fixtures/vdl2-136975k-1.sigmf-meta", "expect": {"icao": "<the aircraft's ICAO hex>"}, "within_ms": 10000},
  "position": {"lat": "lat", "lon": "lon", "alt_m": "alt_ft", "alt_unit": "ft"},
  "time": null,
  "subject": {"key": "icao", "name": "aircraft", "waterfall": "summary",
    "columns": [{"field": "addr_type", "label": "Type", "unit": null}, {"field": "reg", "label": "Reg", "unit": null},
                {"field": "flight", "label": "Flight", "unit": null}, {"field": "label", "label": "Label", "unit": null},
                {"field": "text", "label": "Text", "unit": null}],
    "heading": null, "label": "flight", "merge": ["reg", "icao"], "track_points": 100, "track_min_m": 1000},
  "notes": "..."
}
```

**Channel and rate.** The row asks for a 25 kHz channel. In the field host's 8 MHz capture the
bank serves it from a 2 MHz bin; `channel_rate_hz(2 000 000, 25 000)` is 50 kHz (the narrowest
exact divisor at or over 48 kHz), and the adaptor resamples 50 → 105 kHz (L/M = 21/10). From a
narrow capture without a bank the same rule gives the same shape. dumpvdl2 runs at
`--oversample 1`, 105 kHz: that is its per-channel rate anyway (ten samples a symbol at 10.5
kbaud; at N it decimates each channel by N to 105 kHz), the stream carries only the 25 kHz
channel, centred, so nothing beyond it needs filtering out, and it is the cheapest in CPU on
both sides (no decimation in dumpvdl2, the smallest resampler in the adaptor). The capture's
centre is the job's centre (`--centerfreq $center_hz` and the one channel `$center_hz`), so
dumpvdl2 never retunes or offsets. `S16_LE` is the spelling 2.6.0 accepts (its help says
`S16LE`; constraint above); `cs16` is the adaptor's `x * 32 767`.

**Events and the subject.** Every AVLC frame dumpvdl2 decodes is an event (XID, supervisory,
information frames, ground-station squitters), keyed on the source address (`icao`: an
aircraft's 24-bit ICAO address, a ground station's own address; `addr_type` tells them apart).
An aircraft's row carries its downlinks with `reg`, `flight`, `label`, `text` from the ACARS
block when the frame has one; the ground station (`addr_type` "Ground station") gets a row of
its own whose ACARS columns name the aircraft each uplink is for. The operator does not mind
which way the table is keyed (decision 4), so the default stands. `label` is the subject's name
field (`flight`), `waterfall` is `summary` (one line for the job, as ACARS). `t_ns` is the IQ
frame's time: dumpvdl2's `t` is `{sec, usec}`, not ISO, and the frame time is within a block of
it.

**Positions.** An ADS-C report rides an ACARS downlink (labels A6/B6 and friends) that libacars
decodes into `acars.arinc622.adsc.tags[]`; a periodic or demand report's first tag is the
`basic_report` with `lat`, `lon` (degrees) and `alt` (feet). The map lists the first two tag
slots and the emergency report (the list form, 4.1); a frame with none leaves `lat`/`lon` out
and the event has no `position`, so it updates the subject's state only. `position` converts
feet to metres (`alt_unit: ft`, as `adsb_decode` does). The subject's `track_points` and
`track_min_m` (100 points, 1 km) draw a track per aircraft, and the page's existing subject row
carries the fix, its distance and the job: that is "ADS-B-like details and a link to the job"
with no page change. The exact pointer paths are **to be confirmed on the fixture** (section
6): the nesting is read from libacars's source, not from a captured line.

**Beside ACARS on one radio.** The lease cannot widen a running capture, so the order matters:
`source_pin` the radio at 133 500 000 Hz, 8 000 000 Hz first (130.025–136.975 MHz inside it,
well under the B210's 61.44 MHz), then `plan_start acars_vhf` and `job_start --kind vdl2_decode
--center-hz 136975000 --bandwidth-hz 25000`: every job fits the pin and is leased without a
tune. Started the other way round, VDL2 is a `conflict` naming the seven ACARS jobs, which is
correct and says what to do. The field-craft text says this.

### 4.4 `hfdl_decode`

```json
{
  "id": "hfdl_decode",
  "name": "HFDL aircraft data link (HF)",
  "family": "aviation",
  "band": {"bandwidth_hz": 6000, "centers_hz": [], "min_hz": null, "range_hz": [2800000, 22100000]},
  "modulation": ["psk"],
  "label": "HFDL",
  "protocol": "hfdl",
  "decoder": {
    "kind": "program",
    "programs": ["dumphfdl"],
    "args": ["--iq-file", "-", "--sample-format", "CS16", "--sample-rate", "$rate_hz",
             "--centerfreq", "$center_khz", "--output", "decoded:json:file:path=-", "$center_khz"],
    "input": {"stream": "iq", "format": "cs16", "rate_hz": 12000, "via": "stdin"},
    "output": {"via": "stdout", "port": null, "parser": "json_lines",
      "fields": {
        "icao": ["/hfdl/lpdu/src/ac_info/icao", "/hfdl/lpdu/dst/ac_info/icao"],
        "aircraft_id": ["/hfdl/lpdu/src/id", "/hfdl/lpdu/dst/id"],
        "gs_id": ["/hfdl/lpdu/dst/id", "/hfdl/lpdu/src/id", "/hfdl/spdu/src/id"],
        "gs": ["/hfdl/lpdu/dst/name", "/hfdl/lpdu/src/name", "/hfdl/spdu/src/name"],
        "flight": "/hfdl/lpdu/hfnpdu/flight_id", "pdu": "/hfdl/lpdu/hfnpdu/type/name",
        "lat": "/hfdl/lpdu/hfnpdu/pos/lat", "lon": "/hfdl/lpdu/hfnpdu/pos/lon",
        "reg": "/hfdl/lpdu/hfnpdu/acars/reg", "label": "/hfdl/lpdu/hfnpdu/acars/label", "text": "/hfdl/lpdu/hfnpdu/acars/msg_text",
        "bit_rate": "/hfdl/bit_rate", "sig_level": "/hfdl/sig_level", "freq_skew": "/hfdl/freq_skew"
      }},
    "install": {"debian": null, "url": "https://github.com/szpajder/dumphfdl"}
  },
  "event": {"schema": {"...": "one property per mapped field"}, "summary": "{icao?gs_id} {flight?} {pdu?} {text?}"},
  "identify": {"proof": "event", "min_events": 1, "dwell_ms": 40000, "min_span_ms": 1000, "quick": null},
  "catalogue": ["high-frequency-data-link-hfdl"],
  "test": {"kind": "fixture", "path": "fixtures/hfdl-<freq>k-1.sigmf-meta", "expect": {"gs_id": "<the ground station's number>"}, "within_ms": 10000},
  "position": {"lat": "lat", "lon": "lon", "alt_m": null},
  "time": null,
  "subject": {"key": "icao", "name": "aircraft", "waterfall": "summary",
    "columns": [{"field": "flight", "label": "Flight", "unit": null}, {"field": "gs", "label": "Ground station", "unit": null},
                {"field": "pdu", "label": "PDU", "unit": null}, {"field": "text", "label": "Text", "unit": null}],
    "heading": null, "label": "flight", "merge": ["reg", "icao"], "track_points": 100, "track_min_m": 1000},
  "notes": "..."
}
```

**The channel sits above the listed frequency.** An HFDL channel is listed by its dial
(suppressed-carrier) frequency; the signal is USB on a 1440 Hz subcarrier, 1800 baud, so its
energy lies about 540–2340 Hz above the listed frequency. dumphfdl expects exactly that: given
channel 8942 kHz in a stream centred on 8942 kHz it looks 1.44 kHz up. The job's `center_hz` is
therefore the listed frequency (what the agent reads off a frequency list and what the
catalogue and the register know), and `band.bandwidth_hz` is 6000, not 2400: a 2.4 kHz band
centred on the dial frequency would hold the lower sideband's silence and cut the signal. 6 kHz
(dial ± 3 kHz) holds the channel with margin and fits a Kiwi's 12 kHz band with the dial
frequency as the band's centre. On a Kiwi the channel rate is the source's 12 000 Hz (under the
48 kHz floor the rule returns the source rate), the adaptor passes it through unresampled, and
dumphfdl gets `--sample-rate 12000`, which it accepts; whether it decodes at 12 kHz is confirmed
on the first fixture (section 6; if not, `input.rate_hz` becomes 24 000 and the adaptor
upsamples). On a wider HF source (an SDR at 2 MHz) the rule gives 48 kHz and the adaptor
resamples down to 12 kHz; nothing in the row changes.

**Events and the subject.** dumphfdl prints a line per PDU: ground-station squitters (`spdu`,
every 32 s per station, no aircraft), and `lpdu`s between an aircraft and a station. The
aircraft is an 8-bit `id` assigned at logon; dumphfdl keeps a cache mapping it to the ICAO
address it learnt from the logon (`ac_info.icao`, `--aircraft-cache-ttl`). The subject key is
that ICAO (src for downlinks, dst for uplinks), so one table row per aircraft whichever way the
frame goes; an aircraft whose logon was missed has no `icao` yet and its frames are events about
no subject until the cache catches it. Squitters are events too (`gs`, `pdu`, no subject): on a
quiet channel they are the proof the channel is alive. A ground station is named by its number
(`gs_id`, always in the line) and by `gs`, its name, when dumphfdl has one: the names come from
the system table, which the row does not carry (a non-goal), so the summary, the check's
`expect` and the field craft lean on the number and show the name when it is there. `position`
is mapped from the performance-data and frequency-data PDUs' `pos`; a track on the map per
aircraft (1 km steps: these are airliners hundreds of km out). `identify.dwell_ms` is 40 s so a
dwell holds at least one 32 s HFDL frame; the check's `within_ms` is 10 s because the fixture
is cut so a squitter lands in its first seconds (section 6).

**Frequencies.** `centers_hz` is empty and `range_hz` spans the HF aero bands: the active
frequencies per ground station change with the hour (the system table), so the agent is told to
take them from a list (dumphfdl's `etc/systable.conf` upstream, the operator's own, or the
TODO's three bands near 8.9, 10.0 and 11.4 MHz for the nearest stations) and to start the job on
the dial frequency. A `--system-table` is not needed to decode. The imported catalogue's HFDL
entry lists only 2.885–2.914 and 21.89–22.11 MHz, so `knowledge_lookup` at 8.9, 10.0 or 11.4
MHz does not surface it; the operator left the import as upstream has it (decision 21), and the
field craft's frequency list is the answer.

### 4.5 A source's position

A KiwiSDR is somewhere else, so a signal it hears was received there, not at the station. The
operator wants each Kiwi to carry its own position, read from the Kiwi where it has GPS, else
configured (decision 10).

**Defined by plan 32 (spec section 4.11, plan Task 2), not here.** That one definition holds the
record and its names, and plan 32's Task 2 owns the implementation: `Port::position() ->
Option<SourcePosition>` (default `None`), `SourcePosition {lat, lon, alt_m: Option<f64>, from:
status | configured}` in `types.rs`, `SourceInfo.position` (nullable, `#[serde(default)]`, listed
in `require_worker`), the Kiwi's once-per-connection `GET /status` read (`gps=(lat, lon)`,
`asl=`, `gps_good=`; `(0, 0)` or out of range is none; a hand-written HTTP/1.0 request on a
`TcpStream`, one bounded task that never delays the tune or faults the source, counted in
`Status.status_errors` when it fails; `KiwiConfig.status_url`), and the generic configured
fallback `--source-position ID=LAT,LON[,ALT_M]` for any source id, kept in
`NodeConfig.source_positions` (a `--source-kiwi` tail was rejected: its `/PASSWORD` may contain
a comma). The status wins over the flag when both exist. If plan 30 lands on `main` first, it
implements exactly that shape and plan 32 reuses it as landed; if plan 32 lands first, plan 30's
task reduces to consuming `SourceInfo.position`. The node's own position stays the station
(`station_set`); a source with no position and `Capabilities.remote` false is at the station.

What is plan 30's own:

- How `hfdl_decode` events carry it: they do not carry coordinates. Every frame's provenance
  names its `source`, the per-source feed and `events.log` record it, and the page already
  knows which source a job runs on; a consumer reads `source_list[].position` for that source.
  Nothing in the envelope or the row changes, so an operator's own program row on a Kiwi gets
  the same.
- Not configured on the operator's node by this plan: the operator adds their Kiwi themselves
  (decision 7). The UAT task is theirs: add it as `--source-kiwi`, see its position in
  `source_list` (or give it `--source-position` if its status has none), run `hfdl_decode`.

### 4.6 Fixtures and `modes check`

Neither mode has a synthesiser (D8PSK and HFDL's PSK framing are weeks of work each, and a real
burst is a better proof). Each row's test is a `fixture`: a SigMF pair recorded off the air,
committed to the repository.

**Where.** `modes/fixtures/<mode>-<dial kHz>k-<n>.sigmf-{meta,data}`, beside the rows they
prove (the DSP crate's `crates/zeta-dsp/tests/data/` keeps the layer-1 captures its own tests
load by `CARGO_MANIFEST_DIR`; a mode fixture is loaded by `zeta`, from a row, so it lives with
the rows). `.gitignore` gains `!/modes/fixtures/*.sigmf-data` so the pairs are tracked by an
ordinary `git add`. A row's `fixture.path` is relative, as above. `modes check` resolves a
relative path against `$ZETA_FIXTURES_DIR` when set, else against `modes/` under the current
directory (a checkout root); an absolute path is taken as it is (operator rows). A pair that is
not there is `skipped: fixture <path> not found (ZETA_FIXTURES_DIR unset; looked in <dir>)`, and
`--strict` makes that a fail as it makes every skip a fail (no new rule: `--strict` means "every
row proves itself here"). The deployed binary on the field host is run from its checkout, so
`zeta modes check --strict vdl2_decode` there works without the variable. The embedded-row
check in CI (`crates/zeta/tests/modes_check.rs`) pins both programs to a path none lives at,
so it expects `skipped: dumpvdl2 missing` and `skipped: dumphfdl missing`, not a fixture lookup.

**Size.** Today's fixtures are 35–275 KB each, 1.8 MB in all. Budget: at most 300 KB per
fixture, one fixture per row. `iq read` writes `cf32` at the capture's rate (a minute of an 8
MHz B210 capture is 3.8 GB), so the TODO's "a minute off air with `iq read`" is the raw
recording, not the fixture:

- VDL2: a `capture` job on the 25 kHz box (the page's "Capture bursts", or `job_start --kind
  capture`) writes the gated bursts at the channel rate (50 kHz `cf32`), with pre- and
  post-roll; VDL2 bursts are tens of milliseconds, so three or four of them are under 100 KB.
  The burst gate was built on 30 ms OOK remotes and suits a VDL2 burst; should it clip one,
  the fallback is `iq read` on a narrow pinned capture and a cut by hand.
- HFDL: 60 s of the Kiwi's 12 kHz (5.8 MB `cf32`, an `iq read` of the ring or a whole-window
  `capture`), `dumphfdl --iq-file` over it offline to find the slots that decode, a cut of 5–10
  s with a squitter in its first seconds and an aircraft frame after it, written as `ci8` (the
  file source reads `ci8`; 12 kHz × 10 s × 2 bytes = 240 KB). A one-off conversion script under
  `scripts/` with uv inline metadata, not a node feature.

Each meta carries `zeta:expected` in its annotation as the OOK fixtures do (the event fields
the check expects), and the test's `expect` names the same subset.

**Recording is operator time, so the rows do not wait for it.** The operator decided
(decisions 6 and 7):

- The B210 is **not** re-pinned now. Recording the VDL2 fixture (the radio pinned at 133.5 MHz
  / 8 MHz with the ACARS plan inside it, or the ACARS jobs stopped for a few minutes) is a UAT
  task the operator and the builder work through together later; stopping or re-pinning jobs
  is the operator's call, never the builder's. `vdl2_decode` lands with its check `skipped:
  fixture ... not found` on every host (`--strict` fails it, as it should until the proof
  exists).
- The local KiwiSDR (on the LAN, port 8073) may be used for **testing only**: a one-off HFDL
  fixture recording and ad-hoc runs from a developer shell with a throwaway `zeta` process
  (its own `--state-dir`, `--source-kiwi hf=kiwi-host:8073`), never the deployed node, never a
  unit file, deploy helper or persisted state. Its host name reaches the shell through an
  untracked environment variable (`ZETA_KIWI_HOST`) and nothing tracked; when the variable is
  unset or the Kiwi is unreachable, the fixture task is skipped and `hfdl_decode` lands like
  `vdl2_decode`, check skipped, with the recording as a UAT task.
- The fake-program tests (5.2) prove the rows' maps in CI meanwhile.

**Public.** Before a fixture is committed, run its decoder over it and read every line. Keep
link-layer frames (XID, squitters, logons), position reports and OOOI/airline operational
messages; cut any free-text message (crew text, `5Z`-style company messages, anything with a
name in it). Strip the meta of anything but the SigMF core fields, `zeta:expected` and a
`zeta:source` role (`b210`, `kiwi`): no host names, no receiver or station positions beyond the
demo station's. The row's `expect` names an aircraft's ICAO hex or a ground station's number,
both public. `scripts/public-check.sh` runs over the meta as over any tracked text file.

### 4.7 Install hints and the catalogue

`install` is `{"debian": null, "url": "https://github.com/szpajder/dumpvdl2"}` and the same
for dumphfdl: neither is in Debian or Ubuntu, so the startup handler line reads `missing
(<url>)`. The rows' `notes` add the one sentence that helps: the OpenWebRX+ apt repository
packages `dumpvdl2`, `dumphfdl` and `libacars2`, and both build from source in a few minutes
(libacars first). `ZETA_HANDLER_DUMPVDL2` and `ZETA_HANDLER_DUMPHFDL` name a program outside
`PATH`, by the existing rule (`proc.rs`).

The catalogue needs no edit: a signature's `decoder` is the ready-or-unavailable row whose
`catalogue` lists the entry (`catalogue.rs`, `CatalogueKnowledge::decoder`). The rows list
`vhf-data-link-mode-2-vdl-m2` and `high-frequency-data-link-hfdl`. The HFDL entry's frequency
ranges stay as upstream has them (decision 21).

### 4.8 One aircraft table across the links

The operator wants one aircraft table across `acars_decode` and `vdl2_decode` (decision 20),
merged by a shared field: `reg`, or the ICAO address when both carry it. Today
(`crates/zeta-ui/web/subjects.js`) the `SubjectStore` keeps subjects per job (`jobs: job id →
key → subject`, merged by `store.event(job, payload)`, where the key is `payload[row.subject.key]`),
and the `SubjectsPane` draws one `section.subject-job[data-job]` per running job, with the
row's columns plus `Position`, `Distance`, `Last seen` and `Count`. The map, the waterfall, dim
and drop, selection (`{job, key}`) and the e2e scenarios all work on those per-job subjects.

The merge is a view on top, not a change to the store:

- **On the row.** `subject.merge: ["reg", "icao"]`, optional (`#[serde(default)]`, empty when
  left out, nullable in the schema): the event fields whose equal values mean "the same
  subject", across rows whose subjects share `subject.name` (the plural noun, "aircraft"). Each
  must name a schema property, else `broken` at `subject.merge[i]`. The subject's own key
  counts as a field of its state. `acars_decode` gets `["reg"]` (it has no ICAO), `vdl2_decode`
  and `hfdl_decode` `["reg", "icao"]`. `adsb_decode` is left alone in this plan (adding
  `["icao"]` to it later is one line; the operator asked for ACARS and VDL2).
- **On the page.** When two or more running jobs' rows declare `merge` and share
  `subject.name`, the pane draws one `section.subject-merged[data-name="aircraft"]` for them in
  place of their per-job sections. Its rows are groups of per-job subjects: two subjects are one
  group when, for any field in either row's `merge`, both have a non-blank value and the values
  are equal (the key included; union-find over the running jobs' subjects, recomputed on each
  render, which is already throttled). A group's state is the member states merged
  newest-last-seen wins, its position the newest member's, its `count` the sum, `first_seen`
  the earliest, `last_seen` the latest, `dim` when every member is dim. The key column shows
  the first `merge` field the group has (`reg`, else `icao`); the columns are the union of the
  rows' `columns` by field (`acars` and `vdl2` share `flight`, `label`, `text`), then a `Via`
  column listing the protocols heard (`acars`, `vdl2`), then `Position`, `Distance`, `Last
  seen`, `Count` as today. The heading reads `aircraft, N (acars_decode b210, vdl2_decode
  b210)`. A press on a merged row selects the newest member's `{job, key}` in the store, so the
  map and the waterfall highlight as they do today; the merged row is `selected` when any
  member is. A job whose row has no `merge`, or is alone in its `subject.name`, keeps its
  per-job section unchanged.
- **Agent and CLI.** Nothing: events stay per job and per row, `events.log` and
  `stream_tail` unchanged. The agent sees `merge` in `modes_show` and the guide says what it
  means in one sentence.

**Why a view, not a store change.** Per-job subjects are what every other consumer holds
(selection, tracks, dim/drop, the fold ribbons); merging in the store would change the identity
the map and the waterfall key on. A grouped view costs one function and one section kind, and
falls back to today's tables the moment the rows disagree.

### 4.9 Docs and TODO

- Field craft, "Decoding pagers, aircraft and FM data": a bullet each. VDL2: 136 975 000 Hz
  worldwide (the European channels listed), 25 000 Hz; needs dumpvdl2; beside ACARS pin the
  radio at 133 500 000 Hz / 8 000 000 Hz first, then the ACARS plan and the VDL2 job, in that
  order; report `summary`, that a ground-station row's ACARS columns name the aircraft of each
  uplink, and that an ADS-C report gives the frame a `position`. HFDL: on a Kiwi, `center_hz`
  is the listed (dial) frequency, 6 000 Hz; the signal is 1.44 kHz above it, so a `measure` box
  on the carrier reads 2.4 kHz of PSK offset up; a squitter every 32 s proves the channel;
  frequencies from a list, not a raster. One sentence on `subject.merge` (the page shows one
  aircraft table across the links) and one on a source's `position` in `source_list`
  ("Remote HF receivers (KiwiSDR)").
- The "Set up a background decoder for all ACARS and ADS-B activity" recipe gains the VDL2
  line; plan 31's named set replaces the recipe later.
- `agents_md.rs`: `--source-position` in the flags (shared with plan 32's Task 2; whichever lands first writes it), `$center_khz` in the `args` doc text,
  `fields` and `merge` in the `modes_list`/`modes_show` text, `position` in `source_list`'s.
- TODO: the two source notes are replaced by one line pointing at this spec and the plan, with
  the airband set left to plan 31's gist; new short gists for what this plan leaves (section 6:
  `adsb_decode` in the merge, ADS-C over the ATN half, the VDL2 fixture if still unrecorded).
- UAT: VDL2 on the B210 beside the ACARS plan (recording the fixture first), the operator's own
  Kiwi added as a source with its position and `hfdl_decode` on it (section 5.4).

## 5. Testing

### 5.1 Row schema (`crates/zeta-core`, `row.rs` and `summary.rs` unit tests)

- A `fields` map with a pointer, a list of pointers (first non-null wins, null skipped), a
  pointer to nothing (field absent), a pointer to an object (copied), a pointer with an array
  index (`/a/tags/0/basic_report/lat`).
- `broken` cases: a key not in the schema (`decoder.output.fields.<key>`); a schema property not
  in the map (`event.schema.properties.<name>`), the envelope fields exempt; a pointer not
  starting with `/`; an empty list.
- `summary`, `subject.key` and `position` resolved on a mapped event: `{reg?}` dropped when the
  pointer resolved to nothing; a null `icao` is "no subject"; `alt_unit: ft` on a mapped
  `alt_ft`.
- `subject.merge`: round-trips, defaults to empty, names schema properties (`subject.merge[i]`
  when not), reported by `modes_show`.
- `$center_khz`: 8 942 000 → `8942`; 8 942 500 → `8942.5`; 136 975 000 → `136975`; an unknown
  placeholder is still `broken` at `decoder.args[i]`.
- `tests/modes.rs`: both embedded rows parse and validate against zeta-dsp's stages (`EMBEDDED`
  8 → 10, in `modes/mod.rs` and the test); their `label`s and `protocol`s are pinned
  (`VDL2`/`vdl2`, `HFDL`/`hfdl`); `the_rows_name_the_planned_subjects` covers `merge`.

### 5.2 Adaptor (`crates/zeta-ext`)

- `Publisher::publish` with a map over a captured real dumpvdl2 line (kept as a test vector in
  `crates/zeta-ext/tests/data/`, reviewed as a fixture is; until a real line exists, a line
  built to libacars's documented shape, marked as such in the file) yields `icao`, `reg`,
  `text`, `position` and no `vdl2` key; the same line without a map yields today's whole object.
- `substitute` fills `$center_khz`.
- The existing `fake_lines` (`crates/zeta-ext/tests/fake_lines.py`, `--lines-file PATH`)
  stands in for the programs: an operator-dir copy of `vdl2_decode` with `programs:
  ["fake_lines"]` pinned by `ZETA_HANDLER_FAKE_LINES` (or an absolute path, as the e2e's fake
  rows do) and `args: ["--lines-file", <the vector>]` runs end to end in
  `crates/zeta/tests/modes_check.rs` over any synthesised capture (a `psk` family test): the
  first event's `icao` is the vector's and its `summary` reads as the template says. The same
  for dumphfdl, with the `icao` list form exercised (a downlink line and an uplink line).

### 5.3 `modes check` (`crates/zeta/tests/modes_check.rs`)

- The embedded set with both programs pinned missing prints `vdl2_decode: skipped: dumpvdl2
  missing` and `hfdl_decode: skipped: dumphfdl missing`; `--strict` exits 1 naming them.
- A row whose fixture path does not resolve prints `skipped: fixture <path> not found (...)`,
  and `--strict` fails it; with `ZETA_FIXTURES_DIR` pointing at a directory holding the pair it
  runs. An absolute path is used as given (the existing `a_fixture_test_row_is_checked`).

### 5.4 Source position and the page

- The source position's tests are plan 32's (section 6.2, Task 2): `status::parse` and a
  one-shot HTTP stub (`gps=` and `asl=` give `position()` `{lat, lon, alt_m, from: status}`;
  none without; a status read that fails or hangs leaves the source tuning and faultless);
  `--source-position ID=LAT,LON[,ALT_M]` parses, refuses a bad value or an unknown id naming the
  flag, and `source_list` reports `position` with `from: configured`, `null` for a file source
  without it. Plan 30 adds none of its own beyond reading the field.
- e2e (`ui.py`): `scenario_subjects_merge_across_rows`: two fake rows (`fake_acars`, keyed
  `reg`, `merge: ["reg"]`; `fake_vdl2`, keyed `icao`, `merge: ["reg", "icao"]`, both `name:
  aircraft`) started on the tone source; the pane shows one `section.subject-merged` with one
  row for the aircraft both heard (`Via` reads `fake_acars, fake_vdl2`, `Count` is the sum), one
  for the aircraft only VDL2 heard, and no per-job sections for the two; stopping `fake_vdl2`
  returns `fake_acars` to its own section. Added to the suite's list in `ui.rs`.

### 5.5 Live (UAT, the field host; the operator's radio time)

- Record the VDL2 fixture together: pin the B210 at 133.5 MHz / 8 MHz (the ACARS plan keeps
  running inside), `capture` on 136.975 MHz / 25 kHz, review and commit; then `zeta modes check
  --strict vdl2_decode` from the checkout prints `ok`.
- VDL2: with the pin in place, `plan_start acars_vhf`, start `vdl2_decode` on 136.975 MHz;
  within a minute the Subjects tab shows one aircraft table with `Reg`/`Flight`/`Text` and
  `Via`, the ground station's row with uplinks, a `position` on an ADS-C frame when one is
  heard, `knowledge_lookup` on the band names `vdl2_decode`, and `job_context` shows no
  `external.unparsed` growth.
- The operator's Kiwi: add it to their instance as `--source-kiwi`; `source_list` shows its
  `position` (from its status, else `--source-position`); `hfdl_decode` on one of the nearest station's listed frequencies gives a squitter
  event within 40 s and an aircraft row with a position on the map when one logs on. `zeta
  modes check --strict hfdl_decode` prints `ok` when the HFDL fixture is in.

## 6. Left to the plan

- The exact JSON paths. The pointers above follow the key names found in the binaries,
  libacars's source and the decoders' documented output; the plan's first task confirms them
  against dumpvdl2's and dumphfdl's upstream JSON formatters (the B210 is not available for a
  live VDL2 line now) and against a live dumphfdl line from the local KiwiSDR when the shell
  can reach it, and fixes the schema types. The ADS-C tag wrapper and depth are confirmed on
  the VDL2 fixture; the row's pointers are revised then if they differ. Same for `lat`/`lon`
  being degrees as numbers.
- Whether dumphfdl decodes at 12 kHz input. It accepts the rate; if the fixture does not decode
  at 12 kHz but does at 24 kHz offline, `input.rate_hz` becomes 24 000 and the adaptor
  upsamples (L/M = 2/1). The rate is the row's, so nothing else moves.
- The HFDL fixture's frequency and the field-craft frequency list: taken from dumphfdl's system
  table for the two stations nearest the demo station, at the hour the fixture is recorded.
- `modes check` waits for the first event with one `stream_tail`, capped at `TAIL_MAX_MS`
  (10 s). The HFDL fixture is cut so a squitter lands in its first seconds and `within_ms` is
  10 000 (the row above says so); the check is not changed.
- The Kiwi status page's exact lines (`gps=(lat, lon)`, `asl=`), confirmed on the local
  KiwiSDR from a developer shell; and whether a Kiwi without GPS prints its configured
  location under the same key (expected: yes).
- Whether `fields` lives on `ProgramOutput` as `Option<BTreeMap<String, FieldPath>>` with
  `FieldPath` an untagged `String | Vec<String>`, or a small struct; the row tests are the
  contract.
- The merged table's exact DOM and the `Via` column's text; the e2e scenario is the contract.
- A lone `vdl2_decode` on a direct-conversion radio centres the capture on the channel and puts
  the radio's DC spur mid-channel. Checked once when the VDL2 fixture is recorded (dumpvdl2 has
  no DC-offset option); if it fails, the field craft says to pin the radio off the channel, as
  the ACARS case does anyway, and the general fix (a capture offset for lone jobs) is already a
  TODO gist ("Lease: keep a lone job's capture centre off its channel").
- Order of work: the schema (`fields`, `$center_khz`, `merge`) with its unit tests; the
  fake-program tests; the two rows; the source position; the page's merged table; the HFDL
  fixture when the local Kiwi is reachable; docs and AGENTS.md regeneration; UAT.

## 7. The operator's decisions (2026-11-06)

Numbered as in the sprint's questions to the operator.

4. **Keying.** The operator does not mind which address keys the table: the default stands
   (the AVLC source address; a ground station gets its own row). What they want is aircraft
   positions on the map wherever the link carries one (ADS-C reports libacars decodes inside
   dumpvdl2), with ACARS- and ADS-B-like details (`reg`, `flight`, `label`, `text`) and a link
   to the job. So the row **does** get `position`, mapped from the ARINC 622 ADS-C basic report
   at `/vdl2/avlc/acars/arinc622/adsc/tags/<i>/basic_report/{lat,lon,alt}` (feet; 4.3), the
   exact path to be confirmed on the fixture. Frames without one update the subject only.
6. **The B210 is not re-pinned now.** Recording the VDL2 fixture and the live check are UAT
   tasks worked through together later; the rows land with their fixture checks skipped.
7. **The Kiwi.** The operator will add their own KiwiSDR (on the LAN, port 8073) to their node
   instance themselves; no plan task, deploy helper, unit file or persisted state adds it. The
   plan makes sure a Kiwi they add works (`hfdl_decode` on a `--source-kiwi` source, the
   per-source position). Test use is allowed: a one-off HFDL fixture recording and ad-hoc runs
   from a developer shell with a throwaway `zeta` process, never the deployed node. Its host
   name is private: "the local KiwiSDR" and `kiwi-host` in tracked files. UAT gets "add your
   Kiwi as a source and run `hfdl_decode`".
10. **Kiwi position: yes, now.** Each KiwiSDR source carries its own position, read from the
    Kiwi's status where it has GPS, else configured; signals received through it are placed,
    cut and drawn from the Kiwi's position, not the station's. This spec defines the field on
    the source (4.5, defined by plan 32's section 4.11 and implemented by its Task 2) and how
    events reach it (by provenance); plan 32 draws from it.
20. **One aircraft table across ACARS and VDL2: yes, in this plan.** The page merges the rows'
    subjects by a shared field (`reg`, or ICAO when both carry it); 4.8.
21. **The catalogue's HFDL ranges:** left as upstream.

## 8. Calls made in this document

- Pointers, not flattening (4.1); the list form for "first of"; with a map the event is the
  mapped fields only, and an unmapped schema property is `broken`, the envelope's four fields
  excepted.
- No conditional in the field map. "src when it is an aircraft, else dst" would need a small
  expression language, a worse answer than one more row revision; the list form covers HFDL
  because an uplink's `src.ac_info` is absent, and covers the ADS-C tag slots.
- `$center_khz` as a placeholder rather than a unit suffix on `$center_hz`, to match the
  existing fixed-list style and its validation.
- `--oversample 1` (105 kHz) for VDL2: the cheapest exact per-channel rate, since the node
  delivers only the one channel. Not 10 (1.05 MHz, dumpvdl2's SDR default, which would make the
  adaptor upsample a 25 kHz channel twentyfold for dumpvdl2 to decimate it back).
- `S16_LE`, the spelling 2.6.0 accepts, over `U8` (8-bit would lose dynamic range on a strong
  ground station beside a weak aircraft).
- VDL2 `bandwidth_hz` 25 000 (the channel raster; the D8PSK occupies about 17 kHz), the five
  common channels as `centers_hz`, `range_hz` null (the catalogue's 117.975–137 MHz is the whole
  voice airband and would score every airband signal as plausible VDL2).
- VDL2's key field is named `icao` (the AVLC address is the aircraft's ICAO 24-bit address),
  not `addr`, so the merge field has one name across `vdl2_decode`, `hfdl_decode` and
  `adsb_decode`; `addr_type` says when it is a ground station's.
- VDL2 events: every decoded AVLC frame, not only those with an ACARS block; a frame's `t_ns`
  is the IQ frame's time, not dumpvdl2's `{sec, usec}` (a `time` from a split seconds field is
  not worth a schema feature for a few milliseconds); `position` from the ADS-C basic report
  only (the first two tag slots and the emergency report), altitude in feet converted by
  `alt_unit`.
- HFDL `bandwidth_hz` 6000 centred on the dial frequency, for the reason in 4.4; `position`
  mapped from `hfnpdu.pos` (a structured field; the TODO's "no position" was about ACARS texts),
  `track_min_m` 1000; `identify.dwell_ms` 40 000 to hold one HFDL frame; the check's
  `within_ms` 10 000 with the fixture cut to open on a squitter; squitters are events about no
  subject.
- HFDL `centers_hz` empty with `range_hz` [2.8, 22.1] MHz, the frequency list in prose, since
  the active set follows the system table by hour.
- HFDL ground stations by number (`gs_id`), the name (`gs`) shown when dumphfdl has one, rather
  than carrying a system table for the names (from the builder check).
- The source position as a `Port` accessor plus a generic `--source-position` flag, not a
  Kiwi-only `lat=,lon=` suffix on `--source-kiwi`: any remote source may have one, and the
  Kiwi flag's `/PASSWORD` tail may contain a comma. The Kiwi reads its own from `/status` over a
  plain HTTP request on a `TcpStream`, no HTTP client dependency, and the status wins over the
  flag. One definition, in plan 32 (4.11, Task 2). Events carry the position by provenance
  (`source`), not as a field.
- The merge declared on the rows (`subject.merge`, grouped by `subject.name`) and applied by
  the page as a view over per-job subjects, not a store change and not a page rule naming two
  rows (4.8). `adsb_decode` is not given `merge` in this plan.
- Two landings by default, rows before fixtures, with the fixture checks skipped until the
  operator's radio time (4.6); the VDL2 fixture is a UAT task (decision 6); the HFDL fixture is
  recorded from the local KiwiSDR with a throwaway `zeta` from a developer shell when it is
  reachable, never from the deployed node (decision 7).
- Fixtures under `modes/fixtures/`, relative to `ZETA_FIXTURES_DIR` or `modes/` under the
  current directory, at most 300 KB each, `ci8` allowed, reviewed for content before commit, an
  un-ignore rule for their `.sigmf-data`; a missing fixture is a skip with the lookup named,
  and `--strict` fails it like every skip.
- `install.debian` null with the OpenWebRX+ repository named in `notes`, rather than
  `debian: "dumpvdl2"` (which would print an `apt install` line that fails on a stock Debian).
- The catalogue entries are not edited for `decoder` (derived from the rows) nor for the HFDL
  frequency ranges (decision 21).
- The airband named set (plan 31) and the map side of the source position (plan 32) are out of
  scope and named as such.
